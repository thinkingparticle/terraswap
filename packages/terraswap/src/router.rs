use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use cosmwasm_std::{Uint128};
use cw20::Cw20ReceiveMsg;

use crate::asset::AssetInfo;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct InstantiateMsg {
    pub terraswap_factory: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SwapOperation {
    NativeSwap {
        offer_denom: String,
        ask_denom: String,
    },
    TerraSwap {
        offer_asset_info: AssetInfo,
        ask_asset_info: AssetInfo,
    },
    // todo: maybe take these out from SwapOperation and put them in a BridgeOperation enum
    TerraBridge {
        asset_info: AssetInfo,
        bridge_contract_address: String,
        wallet_address_on_target_chain: String, //
    },
    WormHoleBridge {
        // to be implemented once wormhole live and connected to terra
        asset_info: AssetInfo,
        wallet_address_on_target_chain: String
    },
    IbcTransfer {
        // to be implemented after IBC live on terra
        asset_info: AssetInfo,
        channel_id: String,
        port_id: String,
        wallet_address_on_target_chain: String,
        // for transferring cw20 over ibc:
        // https://github.com/CosmWasm/cw-plus/tree/v0.6.0-beta1/contracts/cw20-ics20
        ics20_contract_address: Option<String>,
        revision_number: Uint128,
        revision_height: Uint128,
    },
}

impl SwapOperation {
    pub fn get_target_asset_info(&self) -> AssetInfo {
        match self {
            SwapOperation::NativeSwap { ask_denom, .. } => AssetInfo::NativeToken {
                denom: ask_denom.clone(),
            },
            SwapOperation::TerraSwap { ask_asset_info, .. } => ask_asset_info.clone(),
            SwapOperation::Bridge { asset_info, .. } => asset_info.clone()
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteMsg {
    Receive(Cw20ReceiveMsg),
    /// Execute multiple BuyOperation
    ExecuteSwapOperations {
        operations: Vec<SwapOperation>,
        minimum_receive: Option<Uint128>,
        to: Option<String>,
    },

    /// Internal use
    /// Swap all offer tokens to ask token
    ExecuteSwapOperation {
        operation: SwapOperation,
        to: Option<String>,
    },
    /// Internal use
    /// Check the swap amount is exceed minimum_receive
    AssertMinimumReceive {
        asset_info: AssetInfo,
        prev_balance: Uint128,
        minimum_receive: Uint128,
        receiver: String,
    },

    /// Execute multiple swaps and bridges
    ExecuteTeleport {
        operations: Vec<SwapOperation>,
        minimum_receive: Option<Uint128>,
        ref_address: Option<String>,
        ref_fee_pct: Option<Uint128>,
        to: Option<String>,
    },

    /// Internal use
    /// Send from contract wallet with charging a fee
    ExecuteSendOrBridgeFromSelfWithFee {
        asset_info: AssetInfo,
        prev_balance: Uint128,
        receiver: String,
        ref_fee_pct: Option<Uint128>,
        ref_address: Option<String>,
        memo: Option<String>
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Cw20HookMsg {
    ExecuteSwapOperations {
        operations: Vec<SwapOperation>,
        minimum_receive: Option<Uint128>,
        to: Option<String>,
    },
    /// Execute multiple swaps and bridges
    ExecuteTeleport {
        operations: Vec<SwapOperation>,
        minimum_receive: Option<Uint128>,
        ref_address: Option<String>,
        ref_fee_pct: Option<Uint128>,
        to: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg {
    Config {},
    SimulateSwapOperations {
        offer_amount: Uint128,
        operations: Vec<SwapOperation>,
    },
}

// We define a custom struct for each query response
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct ConfigResponse {
    pub terraswap_factory: String,
}

// We define a custom struct for each query response
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct SimulateSwapOperationsResponse {
    pub amount: Uint128,
}
