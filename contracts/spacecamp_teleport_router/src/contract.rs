#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;

use cosmwasm_std::{from_binary, to_binary, Addr, Api, Binary, Coin, CosmosMsg, Deps, DepsMut, Env, MessageInfo, QueryRequest, Response, StdError, StdResult, Uint128, WasmMsg, WasmQuery, BankMsg, QuerierWrapper};

use crate::operations::execute_swap_operation;
use crate::querier::compute_tax;
use crate::state::{Config, CONFIG};

use cw20::{Cw20ReceiveMsg, Cw20ExecuteMsg};
use std::collections::HashMap;
use terra_cosmwasm::{SwapResponse, TerraMsgWrapper, TerraQuerier};
use terraswap::asset::{Asset, AssetInfo, PairInfo};
use terraswap::pair::{QueryMsg as PairQueryMsg, SimulationResponse};
use terraswap::querier::query_pair_info;
use terraswap::router::{
    ConfigResponse, Cw20HookMsg, ExecuteMsg, InstantiateMsg, QueryMsg,
    SimulateSwapOperationsResponse, SwapOperation,
};

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> StdResult<Response> {
    CONFIG.save(
        deps.storage,
        &Config {
            terraswap_factory: deps.api.addr_canonicalize(&msg.terraswap_factory)?,
        },
    )?;

    Ok(Response::default())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> StdResult<Response<TerraMsgWrapper>> {
    match msg {
        ExecuteMsg::Receive(msg) => receive_cw20(deps, env, info, msg),
        ExecuteMsg::ExecuteSwapOperations {
            operations,
            minimum_receive,
            to,
        } => {
            let api = deps.api;
            execute_swap_operations(
                deps,
                env,
                info.sender,
                operations,
                minimum_receive,
                optional_addr_validate(api, to)?,
            )
        }
        ExecuteMsg::ExecuteSwapOperation { operation, to } => {
            let api = deps.api;
            execute_swap_operation(
                deps,
                env,
                info,
                operation,
                optional_addr_validate(api, to)?.map(|v| v.to_string()),
            )
        }
        ExecuteMsg::AssertMinimumReceive {
            asset_info,
            prev_balance,
            minimum_receive,
            receiver,
        } => assert_minium_receive(
            deps.as_ref(),
            asset_info,
            prev_balance,
            minimum_receive,
            deps.api.addr_validate(&receiver)?,
        ),
        ExecuteMsg::ExecuteTeleport {
            operations,
            minimum_receive,
            ref_address,
            ref_fee_pct,
            to,
        } => {
            let api = deps.api;
            execute_teleport_operations(
                deps,
                env,
                info.sender,
                operations,
                minimum_receive,
                optional_addr_validate(api, to)?,
                ref_address,
                ref_fee_pct,
            )
        }
        ExecuteMsg::ExecuteSendOrBridgeFromSelfWithFee {
            asset_info,
            prev_balance,
            receiver,
            ref_fee_pct,
            ref_address,
        } => execute_send_from_self_with_fee(deps,
                                             env,
                                             asset_info,
                                             prev_balance,
                                             receiver,
                                             ref_fee_pct,
                                             ref_address)
    }
}

fn execute_send_from_self_with_fee(
    deps: DepsMut,
    env: Env,
    asset_info: AssetInfo,
    prev_balance: Uint128,
    receiver: String,
    ref_fee_pct: Option<Uint128>,
    ref_address: Option<String>) -> StdResult<Response<TerraMsgWrapper>> {
    let referral_is_active = check_referral_params_valid(ref_fee_pct, ref_address)?;

    let self_balance = asset_info.query_pool(&deps.querier, deps.api, env.contract.address.clone())?;
    let token_amount_before_fee = (self_balance - prev_balance);

    // receiver_amount = token_amount_before_fee * (1- referral)
    let mut receiver_amount = token_amount_before_fee.clone();
    let mut fee_amount = Uint128::zero();

    if referral_is_active && ref_fee_pct.is_some() && ref_fee_pct.unwrap() > Uint128::zero() {
        receiver_amount = token_amount_before_fee.multiply_ratio(Uint128::new(100) - ref_fee_pct.unwrap(), Uint128::new(100));
        fee_amount = (token_amount_before_fee - receiver_amount);
        if fee_amount.is_zero() {
            receiver_amount = token_amount_before_fee;
        }
    }

    let asset_to_receiver = Asset {
        info: asset_info.clone(),
        amount: receiver_amount,
    };

    let send_asset_to_receiver_msg = create_transfer_msg(&asset_to_receiver,
                                                         receiver.clone(),
                                                         &deps.querier)?;

    let mut messages: Vec<CosmosMsg<TerraMsgWrapper>> = vec![send_asset_to_receiver_msg];

    if fee_amount > Uint128::zero() {
        let asset_to_referrer = Asset {
            info: asset_info.clone(),
            amount: fee_amount,
        };
        let send_asset_to_referrer_msg = create_transfer_msg(&asset_to_referrer,
                                                             receiver.clone(),
                                                             &deps.querier)?;
        messages.push(send_asset_to_referrer_msg);
    }

    return Ok(Response::new().add_messages(messages));
}

fn create_transfer_msg(
    asset: &Asset,
    recipient: String,
    querier: &QuerierWrapper,
) -> StdResult<CosmosMsg<TerraMsgWrapper>> {
    return match &asset.info {
        AssetInfo::Token { contract_addr } => Ok(WasmMsg::Execute {
            contract_addr: contract_addr.clone(),
            msg: to_binary(&Cw20ExecuteMsg::Transfer { recipient: recipient, amount: asset.amount })?,
            funds: vec![],
        }.into()),
        AssetInfo::NativeToken { .. } => Ok(BankMsg::Send {
            to_address: recipient,
            amount: vec![asset.deduct_tax(querier)?],
        }.into())
    };
}

fn optional_addr_validate(api: &dyn Api, addr: Option<String>) -> StdResult<Option<Addr>> {
    let addr = if let Some(addr) = addr {
        Some(api.addr_validate(&addr)?)
    } else {
        None
    };

    Ok(addr)
}

pub fn receive_cw20(
    deps: DepsMut,
    env: Env,
    _info: MessageInfo,
    cw20_msg: Cw20ReceiveMsg,
) -> StdResult<Response<TerraMsgWrapper>> {
    let sender = deps.api.addr_validate(&cw20_msg.sender)?;
    match from_binary(&cw20_msg.msg)? {
        Cw20HookMsg::ExecuteSwapOperations {
            operations,
            minimum_receive,
            to,
        } => {
            let api = deps.api;
            execute_swap_operations(
                deps,
                env,
                sender,
                operations,
                minimum_receive,
                optional_addr_validate(api, to)?,
            )
        }
        Cw20HookMsg::ExecuteTeleport {
            operations,
            minimum_receive,
            ref_address,
            ref_fee_pct,
            to,
        } => {
            let api = deps.api;
            execute_teleport_operations(
                deps,
                env,
                _info.sender,
                operations,
                minimum_receive,
                optional_addr_validate(api, to)?,
                ref_address,
                ref_fee_pct,
            )
        }
    }
}

pub fn execute_swap_operations(
    deps: DepsMut,
    env: Env,
    sender: Addr,
    operations: Vec<SwapOperation>,
    minimum_receive: Option<Uint128>,
    to: Option<Addr>,
) -> StdResult<Response<TerraMsgWrapper>> {
    let operations_len = operations.len();
    if operations_len == 0 {
        return Err(StdError::generic_err("must provide operations"));
    }

    // Assert the operations are properly set
    assert_operations(&operations)?;

    let to = if let Some(to) = to { to } else { sender };
    let target_asset_info = operations.last().unwrap().get_target_asset_info();

    let mut operation_index = 0;
    let mut messages: Vec<CosmosMsg<TerraMsgWrapper>> = operations
        .into_iter()
        .map(|op| {
            operation_index += 1;
            Ok(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: env.contract.address.to_string(),
                funds: vec![],
                msg: to_binary(&ExecuteMsg::ExecuteSwapOperation {
                    operation: op,
                    to: if operation_index == operations_len {
                        Some(to.to_string())
                    } else {
                        None
                    },
                })?,
            }))
        })
        .collect::<StdResult<Vec<CosmosMsg<TerraMsgWrapper>>>>()?;

    // Execute minimum amount assertion
    if let Some(minimum_receive) = minimum_receive {
        let receiver_balance = target_asset_info.query_pool(&deps.querier, deps.api, to.clone())?;

        messages.push(CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr: env.contract.address.to_string(),
            funds: vec![],
            msg: to_binary(&ExecuteMsg::AssertMinimumReceive {
                asset_info: target_asset_info,
                prev_balance: receiver_balance,
                minimum_receive,
                receiver: to.to_string(),
            })?,
        }))
    }

    Ok(Response::new().add_messages(messages))
}

pub fn execute_teleport_operations(
    deps: DepsMut,
    env: Env,
    sender: Addr,
    operations: Vec<SwapOperation>,
    minimum_receive: Option<Uint128>,
    to: Option<Addr>,
    ref_address: Option<String>,
    ref_fee_pct: Option<Uint128>,
) -> StdResult<Response<TerraMsgWrapper>> {
    let operations_len = operations.len();
    if operations_len == 0 {
        return Err(StdError::generic_err("must provide operations"));
    }

    // Assert the operations are properly set
    let swaps: Vec<SwapOperation> = operations.iter().cloned().filter(
        |o| match o {
            SwapOperation::NativeSwap { .. } => true,
            SwapOperation::TerraSwap { .. } => true,
            _ => false
        }
    ).collect();
    // assert_operations(&operations)?;
    assert_operations(&swaps)?;

    let referral_is_active = check_referral_params_valid(ref_fee_pct.clone(),
                                                         ref_address.clone())?;
    let bridge_operation: Option<SwapOperation> = check_for_valid_bridge_operation(&operations, deps.api)?;
    let have_valid_bridge_operation = bridge_operation.is_some();

    let to = if let Some(to) = to { to } else { sender };
    let target_asset_info = swaps.last().unwrap().get_target_asset_info();

    let mut messages: Vec<CosmosMsg<TerraMsgWrapper>> = vec![];

    let mut operation_index = 0;
    let swaps_len = swaps.len();
    let mut swap_messages: Vec<CosmosMsg<TerraMsgWrapper>> = swaps
        .into_iter()
        .map(|op| {
            operation_index += 1;
            Ok(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: env.contract.address.to_string(),
                funds: vec![],
                msg: to_binary(&ExecuteMsg::ExecuteSwapOperation {
                    operation: op,
                    to: if operation_index == swaps_len {
                        if referral_is_active == true || have_valid_bridge_operation == true {
                            None // for sending referral or bridge, we should keep the output in contract
                        } else {
                            Some(to.to_string())
                        }
                    } else {
                        None
                    },
                })?,
            }))
        })
        .collect::<StdResult<Vec<CosmosMsg<TerraMsgWrapper>>>>()?;
    messages.append(&mut swap_messages);

    // check for Terra Bridge operation
    // let (wallet_address_on_target_chain, bridge_contract_address_on_terra) = match bridge_operation {
    //     None => (None, None),
    //     _ => return Err(StdError::generic_err(
    //         format!("unsupported bridge operation! operation is {:?}", bridge_operation)))
    // };
    let wallet_address_on_target_chain: Option<String> = None;
    let bridge_contract_address_on_terra: Option<String> = None;

    // when we bridge, the receiving address is the bridge contract.
    let final_receiving_address = if have_valid_bridge_operation
    { bridge_contract_address_on_terra.unwrap() } else { to.to_string() };

    if referral_is_active == true || have_valid_bridge_operation == true {
        // if referral is active, we have kept the coins in contract, should send them from this contract
        let self_balance = target_asset_info.query_pool(&deps.querier, deps.api, env.contract.address.clone())?;
        messages.push(CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr: env.contract.address.to_string(),
            funds: vec![],
            msg: to_binary(&ExecuteMsg::ExecuteSendOrBridgeFromSelfWithFee {
                asset_info: target_asset_info.clone(),
                prev_balance: self_balance,
                receiver: final_receiving_address.clone(),
                ref_fee_pct: ref_fee_pct,
                ref_address: ref_address,
            })?,
        }))
    }

    // Execute minimum amount assertion
    if let Some(minimum_receive) = minimum_receive {
        let receiver_balance = target_asset_info.query_pool(&deps.querier, deps.api, to.clone())?;

        messages.push(CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr: env.contract.address.to_string(),
            funds: vec![],
            msg: to_binary(&ExecuteMsg::AssertMinimumReceive {
                asset_info: target_asset_info,
                prev_balance: receiver_balance,
                minimum_receive,
                receiver: final_receiving_address.clone(),
            })?,
        }))
    }

    Ok(Response::new().add_messages(messages))
}


static REFERRAL_PCT_MAX: u128 = 10; // set maximum referral amount

fn check_referral_params_valid(ref_fee_pct: Option<Uint128>,
                               ref_address: Option<String>) -> StdResult<bool> {
    // todo: maybe check if for very small swpas, chargin a referral causes a fail because of tax;
    // todo: maybe get maximum tax cap and then check if charging a tax fails the tx?;
    let mut referral_is_valid: bool = false;
    if let Some(ref_fee_pct) = ref_fee_pct {
        if let Some(ref_address) = ref_address {
            if ref_address.is_empty() == false {
                if ref_fee_pct > Uint128::new(REFERRAL_PCT_MAX) { // todo: double check this
                    return Err(StdError::generic_err(
                        format!("referral fee should be less than {} percent, but received: {:?}",
                                REFERRAL_PCT_MAX, ref_fee_pct)));
                }
                referral_is_valid = true;
            }
        }
    }
    Ok(referral_is_valid)
}

fn check_for_valid_bridge_operation(operations: &Vec<SwapOperation>, api: &dyn Api) -> StdResult<Option<SwapOperation>> {
    return Ok(None); // todo: revisit after wormhole and IBC
    // let bridge_operations: Vec<SwapOperation> = operations.iter()
    //     .filter(|o| matches!(o,SwapOperation::TerraBridge )).collect();
    // // todo: check that we only have one bridge of any types of terra bridge, wormhole and IBC
    // if bridge_operations.len() > 1 {
    //     return Err(StdError::generic_err(
    //         format!("only one bridge operation is allowed at max but given {}", bridge_operations.len())));
    // }
    // if bridge_operations.len() == 0 {
    //     return Ok(None);
    // }
    // let bridge_operation = bridge_operations.first().unwrap();
    // match bridge_operation {
    //     // api.addr_validate(bridge_operation.)
    //     // SwapOperation::WormHoleBridge { .. } => {} // todo
    //     // SwapOperation::IbcTransfer { .. } => {} // todo
    //     _ => return Err(StdError::generic_err("unsupported bridge type"))
    // }
}


fn assert_minium_receive(
    deps: Deps,
    asset_info: AssetInfo,
    prev_balance: Uint128,
    minium_receive: Uint128,
    receiver: Addr,
) -> StdResult<Response<TerraMsgWrapper>> {
    let receiver_balance = asset_info.query_pool(&deps.querier, deps.api, receiver)?;
    let swap_amount = receiver_balance.checked_sub(prev_balance)?;

    if swap_amount < minium_receive {
        return Err(StdError::generic_err(format!(
            "assertion failed; minimum receive amount: {}, swap amount: {}",
            minium_receive, swap_amount
        )));
    }

    Ok(Response::default())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => to_binary(&query_config(deps)?),
        QueryMsg::SimulateSwapOperations {
            offer_amount,
            operations,
        } => to_binary(&simulate_swap_operations(deps, offer_amount, operations)?),
    }
}

pub fn query_config(deps: Deps) -> StdResult<ConfigResponse> {
    let state = CONFIG.load(deps.storage)?;
    let resp = ConfigResponse {
        terraswap_factory: deps
            .api
            .addr_humanize(&state.terraswap_factory)?
            .to_string(),
    };

    Ok(resp)
}

fn simulate_swap_operations(
    deps: Deps,
    offer_amount: Uint128,
    operations: Vec<SwapOperation>,
) -> StdResult<SimulateSwapOperationsResponse> {
    let config: Config = CONFIG.load(deps.storage)?;
    let terraswap_factory = deps.api.addr_humanize(&config.terraswap_factory)?;
    let terra_querier = TerraQuerier::new(&deps.querier);

    let operations_len = operations.len();
    if operations_len == 0 {
        return Err(StdError::generic_err("must provide operations"));
    }

    let mut operation_index = 0;
    let mut offer_amount = offer_amount;
    for operation in operations.into_iter() {
        operation_index += 1;

        match operation {
            SwapOperation::NativeSwap {
                offer_denom,
                ask_denom,
            } => {
                // Deduct tax before query simulation
                // because last swap is swap_send
                if operation_index == operations_len {
                    offer_amount = offer_amount.checked_sub(compute_tax(
                        &deps.querier,
                        offer_amount,
                        offer_denom.clone(),
                    )?)?;
                }

                let res: SwapResponse = terra_querier.query_swap(
                    Coin {
                        denom: offer_denom,
                        amount: offer_amount,
                    },
                    ask_denom,
                )?;

                offer_amount = res.receive.amount;
            }
            SwapOperation::TerraSwap {
                offer_asset_info,
                ask_asset_info,
            } => {
                let pair_info: PairInfo = query_pair_info(
                    &deps.querier,
                    terraswap_factory.clone(),
                    &[offer_asset_info.clone(), ask_asset_info.clone()],
                )?;

                // Deduct tax before querying simulation
                if let AssetInfo::NativeToken { denom } = offer_asset_info.clone() {
                    offer_amount = offer_amount.checked_sub(compute_tax(
                        &deps.querier,
                        offer_amount,
                        denom,
                    )?)?;
                }

                let mut res: SimulationResponse =
                    deps.querier.query(&QueryRequest::Wasm(WasmQuery::Smart {
                        contract_addr: pair_info.contract_addr.to_string(),
                        msg: to_binary(&PairQueryMsg::Simulation {
                            offer_asset: Asset {
                                info: offer_asset_info,
                                amount: offer_amount,
                            },
                        })?,
                    }))?;

                // Deduct tax after querying simulation
                if let AssetInfo::NativeToken { denom } = ask_asset_info {
                    res.return_amount = res.return_amount.checked_sub(compute_tax(
                        &deps.querier,
                        res.return_amount,
                        denom,
                    )?)?;
                }

                offer_amount = res.return_amount;
            }
            SwapOperation::WormHoleBridge { .. } => {} // todo
            SwapOperation::IbcTransfer { .. } => {} // todo
        }
    }

    Ok(SimulateSwapOperationsResponse {
        amount: offer_amount,
    })
}

fn assert_operations(operations: &[SwapOperation]) -> StdResult<()> {
    let mut ask_asset_map: HashMap<String, bool> = HashMap::new();
    for operation in operations.iter() {
        let (offer_asset, ask_asset) = match operation {
            SwapOperation::NativeSwap {
                offer_denom,
                ask_denom,
            } => (
                AssetInfo::NativeToken {
                    denom: offer_denom.clone(),
                },
                AssetInfo::NativeToken {
                    denom: ask_denom.clone(),
                },
            ),
            SwapOperation::TerraSwap {
                offer_asset_info,
                ask_asset_info,
            } => (offer_asset_info.clone(), ask_asset_info.clone()),
            SwapOperation::WormHoleBridge { .. } => return Err(StdError::generic_err("not implemented")),// todo
            SwapOperation::IbcTransfer { .. } => return Err(StdError::generic_err("not implemented"))// todo
        };

        ask_asset_map.remove(&offer_asset.to_string());
        ask_asset_map.insert(ask_asset.to_string(), true);
    }

    if ask_asset_map.keys().len() != 1 {
        return Err(StdError::generic_err(
            "invalid operations; multiple output token",
        ));
    }

    Ok(())
}

#[test]
fn test_invalid_operations() {
    // empty error
    assert!(assert_operations(&[]).is_err());

    // uluna output
    assert!(assert_operations(&vec![
        SwapOperation::NativeSwap {
            offer_denom: "uusd".to_string(),
            ask_denom: "uluna".to_string(),
        },
        SwapOperation::TerraSwap {
            offer_asset_info: AssetInfo::NativeToken {
                denom: "ukrw".to_string(),
            },
            ask_asset_info: AssetInfo::Token {
                contract_addr: "asset0001".to_string(),
            },
        },
        SwapOperation::TerraSwap {
            offer_asset_info: AssetInfo::Token {
                contract_addr: "asset0001".to_string(),
            },
            ask_asset_info: AssetInfo::NativeToken {
                denom: "uluna".to_string(),
            },
        },
    ])
        .is_ok());

    // asset0002 output
    assert!(assert_operations(&vec![
        SwapOperation::NativeSwap {
            offer_denom: "uusd".to_string(),
            ask_denom: "uluna".to_string(),
        },
        SwapOperation::TerraSwap {
            offer_asset_info: AssetInfo::NativeToken {
                denom: "ukrw".to_string(),
            },
            ask_asset_info: AssetInfo::Token {
                contract_addr: "asset0001".to_string(),
            },
        },
        SwapOperation::TerraSwap {
            offer_asset_info: AssetInfo::Token {
                contract_addr: "asset0001".to_string(),
            },
            ask_asset_info: AssetInfo::NativeToken {
                denom: "uluna".to_string(),
            },
        },
        SwapOperation::TerraSwap {
            offer_asset_info: AssetInfo::NativeToken {
                denom: "uluna".to_string(),
            },
            ask_asset_info: AssetInfo::Token {
                contract_addr: "asset0002".to_string(),
            },
        },
    ])
        .is_ok());

    // multiple output token types error
    assert!(assert_operations(&vec![
        SwapOperation::NativeSwap {
            offer_denom: "uusd".to_string(),
            ask_denom: "ukrw".to_string(),
        },
        SwapOperation::TerraSwap {
            offer_asset_info: AssetInfo::NativeToken {
                denom: "ukrw".to_string(),
            },
            ask_asset_info: AssetInfo::Token {
                contract_addr: "asset0001".to_string(),
            },
        },
        SwapOperation::TerraSwap {
            offer_asset_info: AssetInfo::Token {
                contract_addr: "asset0001".to_string(),
            },
            ask_asset_info: AssetInfo::NativeToken {
                denom: "uaud".to_string(),
            },
        },
        SwapOperation::TerraSwap {
            offer_asset_info: AssetInfo::NativeToken {
                denom: "uluna".to_string(),
            },
            ask_asset_info: AssetInfo::Token {
                contract_addr: "asset0002".to_string(),
            },
        },
    ])
        .is_err());
}
