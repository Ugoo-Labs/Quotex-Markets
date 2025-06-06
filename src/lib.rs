use bincode;
use candid::{CandidType, Decode, Encode, Principal};
use ic_cdk::call::Call;
use ic_cdk::{export_candid, storage};
use serde::{Deserialize, Serialize};

use ic_cdk_timers::TimerId;

use sha2::{Digest, Sha256};

use corelib::calc_lib::{_calc_interest, _percentage};
use corelib::constants::{_ONE_PERCENT, _PRICE_FACTOR};
use corelib::order_lib::{CloseOrderParams, LimitOrder, OpenOrderParams};
use corelib::price_lib::_equivalent;
use corelib::swap_lib::{SwapParams, _get_best_offer};
use corelib::tick_lib::{_compressed_tick, _def_max_tick};
use types::{
    FundingRateTracker, GetExchangeRateRequest, GetExchangeRateResult, MarketDetails, StateDetails,
    TickDetails,
};

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Debug;
use std::time::Duration;

use ic_stable_structures::memory_manager::{MemoryId, MemoryManager, VirtualMemory};
use ic_stable_structures::{storable::Bound, Storable};
use ic_stable_structures::{DefaultMemoryImpl, StableBTreeMap, StableCell};

type Time = u64;
type Amount = u128;
type Tick = u64;
type Subaccount = [u8; 32];

type Memory = VirtualMemory<DefaultMemoryImpl>;

const _ADMIN_MEMORY: MemoryId = MemoryId::new(1);

const _MARKET_DETAILS_MEMORY: MemoryId = MemoryId::new(2);

const _STATE_DETAILS_MEMORY: MemoryId = MemoryId::new(3);

const _TICKS_DETAILS_MEMORY: MemoryId = MemoryId::new(4);

const _INTEGRALS_BITMAPS_MEMORY: MemoryId = MemoryId::new(5);

const _FUNDING_RATE_TRACKER_MEMORY: MemoryId = MemoryId::new(6);

const _ACCOUNTS_POSITION_MEMORY: MemoryId = MemoryId::new(7);

const _ACCOUNT_ERROR_LOGS_MEMORY: MemoryId = MemoryId::new(8);

//const _EXECUTABLE_ORDERS_MEMORY: MemoryId = MemoryId::new(9);

const ONE_SECOND: u64 = 1_000_000_000;

const ONE_HOUR: u64 = 3_600_000_000_000;

const _DEFAULT_SWAP_SLIPPAGE: u64 = 30_000; //0.3%

thread_local! {

    static MEMORY_MANAGER:RefCell<MemoryManager<DefaultMemoryImpl>> = RefCell::new(MemoryManager::init(DefaultMemoryImpl::default())) ;

    static ADMIN:RefCell<StableCell<Principal,Memory>> = RefCell::new(StableCell::new(MEMORY_MANAGER.with(|s|{
        s.borrow().get(_ADMIN_MEMORY)
    }),Principal::anonymous()).unwrap());


    static MARKET_DETAILS:RefCell<StableCell<MarketDetails,Memory>> = RefCell::new(StableCell::new(MEMORY_MANAGER.with(|s|{
        s.borrow().get(_MARKET_DETAILS_MEMORY)
    }),MarketDetails::default()).unwrap());


        /// State details
    static STATE_DETAILS:RefCell<StableCell<StateDetails,Memory>> = RefCell::new(StableCell::new(MEMORY_MANAGER.with(|s|{
        s.borrow().get(_STATE_DETAILS_MEMORY)
    }),StateDetails::default()).unwrap());


    static TICKS_DETAILS:RefCell<StableBTreeMap<Tick,TickDetails,Memory>>= RefCell::new(StableBTreeMap::init(MEMORY_MANAGER.with_borrow(
        |mem|{mem.get(_TICKS_DETAILS_MEMORY)})));


    static INTEGRAL_BITMAPS:RefCell<StableBTreeMap<u64,u128,Memory>>= RefCell::new(StableBTreeMap::init(MEMORY_MANAGER.with_borrow_mut(|mem|{
        mem.get(_INTEGRALS_BITMAPS_MEMORY)
    }))) ;


    static FUNDING_RATE_TRACKER:RefCell<StableCell<FundingRateTracker,Memory>> = RefCell::new(StableCell::new(MEMORY_MANAGER.with(|s|{
        s.borrow().get(_FUNDING_RATE_TRACKER_MEMORY)
    }),FundingRateTracker::default()).unwrap());

    static ACCOUNTS_POSITION:RefCell<StableBTreeMap<Subaccount,PositionParameters,Memory>> = RefCell::new(
        StableBTreeMap::init(MEMORY_MANAGER.with(|s|{
        s.borrow().get(_ACCOUNTS_POSITION_MEMORY)
    })));


    static ACCOUNTS_ERROR_LOGS:RefCell<StableBTreeMap<Subaccount,PositionUpdateErrorLog,Memory>> = RefCell::new(
        StableBTreeMap::init(MEMORY_MANAGER.with(|s|{
        s.borrow().get(_ACCOUNT_ERROR_LOGS_MEMORY)
    })));

    static EXECUTABLE_LIMIT_ORDERS_ACCOUNTS:RefCell<Vec<Subaccount>> = RefCell::new(Vec::new());

    static PENDING_TIMER:RefCell<TimerId>= RefCell::new(TimerId::default());

    static LIMIT_ORDERS_RECORD :RefCell<HashMap<Tick,Vec<Subaccount>>> = RefCell::new(HashMap::new());

    static HIGHEST_BUY_OFFER:RefCell<Tick> = RefCell::new(0);

    static LOWEST_SELL_OFFER:RefCell<Tick> = RefCell::new(0);

}

//////////////////////////////////////////////////////////////////////////////////////////////
///////////////////////////////////////////////////////////////////////////////////////////////
/// System Functions
//////////////////////////////////////////////////////////////////////////////////////////////
/////////////////////////////////////////////////////////////////////////////////////////////////
#[ic_cdk::init]
fn init(market_details: MarketDetails) {
    let caller = ic_cdk::api::msg_caller();

    ADMIN.with(|ref_admin| ref_admin.borrow_mut().set(caller).unwrap());
    MARKET_DETAILS.with(|ref_market_details| {
        ref_market_details.borrow_mut().set(market_details).unwrap();
    });
}
/////////////////////////////////////////////////////////////////////////
////////////////////////////////////////////////////////////
/////////////////////////////////////////////////////////
#[ic_cdk::pre_upgrade]
fn pre_upgrade() {
    let limit_orders_accounts_record: HashMap<u64, Vec<[u8; 32]>> =
        LIMIT_ORDERS_RECORD.with_borrow(|reference| reference.clone());

    let executable_orders =
        EXECUTABLE_LIMIT_ORDERS_ACCOUNTS.with_borrow(|reference| reference.clone());

    let highest_buy_offer = _get_highest_buy_offer_tick();
    let lowest_sell_offer = _get_lowest_sell_offer_tick();
    storage::stable_save((
        limit_orders_accounts_record,
        executable_orders,
        highest_buy_offer,
        lowest_sell_offer,
    ))
    .expect("error storing data");
}

#[ic_cdk::post_upgrade]
fn post_upgrade() {
    let limit_orders_accounts_record: HashMap<u64, Vec<Subaccount>>;

    let executable_orders: Vec<Subaccount>;

    let highest_buy_offer: Tick;

    let lowest_sell_offer: Tick;

    (
        limit_orders_accounts_record,
        executable_orders,
        highest_buy_offer,
        lowest_sell_offer,
    ) = storage::stable_restore().unwrap();

    LIMIT_ORDERS_RECORD.with_borrow_mut(|reference| {
        *reference = limit_orders_accounts_record;
    });
    EXECUTABLE_LIMIT_ORDERS_ACCOUNTS.with_borrow_mut(|reference| {
        *reference = executable_orders;
    });

    // let StateDetails { current_tick, .. } = _get_state_details();

    _update_lowest_sell_offer_tick(lowest_sell_offer);

    _update_highest_buy_offer_tick(highest_buy_offer);
}

/// Get State Details
///
/// Returns the Current State Details
#[ic_cdk::query(name = "getStateDetails")]
fn get_state_details() -> StateDetails {
    _get_state_details()
}

/// Get Market Details
///
///  Returns the Market Details
#[ic_cdk::query(name = "getMarketDetails")]
fn get_market_details() -> MarketDetails {
    _get_market_details()
}

#[ic_cdk::query(name = "getAccountPositionDetails")]
fn get_account_position_details(
    user: Principal,
    account_index: u8,
) -> Option<(PositionParameters, PositionStatus, i64)> {
    let account = user._to_subaccount(account_index);
    let Some(position_params) = _get_account_position(&account) else {
        return None;
    };

    let position_status = _convert_account_limit_position_to_market(account, true);

    let StateDetails {
        max_leveragex10, ..
    } = _get_state_details();

    let initial_collateral = position_params.collateral_value as i128;

    let (to_liquidate, current_collateral_int, _) =
        _liquidation_status(position_params, max_leveragex10);

    let pnl = if to_liquidate {
        -100 * _ONE_PERCENT as i64
    } else {
        (((current_collateral_int - initial_collateral) * 100 * _ONE_PERCENT as i128)
            / initial_collateral) as i64
    };
    return Some((position_params, position_status, pnl));
}

#[ic_cdk::query(name = "getBestOffers")]
fn get_best_offers() -> (Tick, Tick) {
    let MarketDetails { tick_spacing, .. } = _get_market_details();

    let lowest_sell_offer_tick = _get_lowest_sell_offer_tick();

    let highest_buy_offer_tick = _get_highest_buy_offer_tick();

    return (
        highest_buy_offer_tick * tick_spacing,
        lowest_sell_offer_tick * tick_spacing,
    );
}

/// Open PositionDetails functions
///
/// opens a new position for user (given that user has no existing position)
/// - Collateral Value :: The amount in collatreal token to utilise as collateral
/// - Max Tick :: max executing tick ,also seen as max price fro the _swap ,if set to none or set outside the required range ,default max tick is used
/// - Leverage :: The leverage for the required position multiplies by 10 i.e a 1.5 levarage is 1.5 * 10 = 15
/// - Long :: Indicating if its a long position or not ,true if long and false otherwise
/// - Order Type :: the type of order to create
///
/// Returns
///  - Position:the details of the position
///
/// Note
///  - If Order type is a limit order ,max tick coinsides with the reference tick for the limit order
///  - ANON TICKS are for future purposes and have no effect for now
#[ic_cdk::update(name = "openLimitPosition")]
async fn open_limit_position(
    account_index: u8,
    long: bool,
    collateral_value: Amount,
    leveragex10: u8,
    max_tick: Tick,
) -> Result<PositionParameters, &'static str> {
    let user = ic_cdk::api::msg_caller();

    let account = user._to_subaccount(account_index);

    let debt_value = (u128::from(leveragex10 - 10) * collateral_value) / 10;

    let market_details = _get_market_details();

    let vault = Vault::init(market_details.vault_id);

    let MarketDetails { tick_spacing, .. } = _get_market_details();
    let entry_tick = _compressed_tick(max_tick, tick_spacing);

    let interest_rate =
        match _open_position_checks(user, account, vault, collateral_value, leveragex10).await {
            Err(error) => return Err(error),
            Ok(rate) => rate,
        };

    let path = || -> Option<(PositionParameters, Vec<Tick>)> {
        if long {
            _open_limit_long_position(
                account,
                collateral_value,
                debt_value,
                interest_rate,
                entry_tick,
            )
        } else {
            _open_limit_short_position(
                account,
                collateral_value,
                debt_value,
                interest_rate,
                entry_tick,
            )
        }
    };

    let Some((position, _)) = path() else {
        vault.manage_position_update(
            user,
            account_index,
            collateral_value,
            ManageDebtParams::init(debt_value, debt_value, debt_value),
        );

        return Err("Failed to open position");
    };
    store_tick_order(max_tick, account);
    return Ok(position);
}

#[ic_cdk::update(name = "openMarketPosition")]
async fn open_market_position(
    account_index: u8,
    long: bool,
    collateral_value: Amount,
    leveragex10: u8,
    max_tick: Option<Tick>,
) -> Result<PositionParameters, &'static str> {
    let user = ic_cdk::api::msg_caller();

    let account = user._to_subaccount(account_index);

    let debt_value = (u128::from(leveragex10 - 10) * collateral_value) / 10;

    let market_details = _get_market_details();

    let vault = Vault::init(market_details.vault_id);

    let interest_rate =
        match _open_position_checks(user, account, vault, collateral_value, leveragex10).await {
            Err(error) => return Err(error),
            Ok(rate) => rate,
        };

    let path = || -> Option<(PositionParameters, Vec<Tick>)> {
        if long {
            _open_market_long_position(
                account,
                collateral_value,
                debt_value,
                interest_rate,
                max_tick,
            )
        } else {
            _open_market_short_position(
                account,
                collateral_value,
                debt_value,
                interest_rate,
                max_tick,
            )
        }
    };

    let Some((position, crossed_ticks)) = path() else {
        vault.manage_position_update(
            user,
            account_index,
            collateral_value,
            ManageDebtParams::init(debt_value, debt_value, debt_value),
        );

        return Err("Failed to open position");
    };

    _schedule_execution_for_ticks_orders(crossed_ticks);

    if position.debt_value != debt_value {
        let un_used_collateral = collateral_value - position.collateral_value;
        vault.manage_position_update(
            user,
            account_index,
            un_used_collateral,
            ManageDebtParams::init(debt_value, debt_value, debt_value - position.debt_value),
        );
    }
    return Ok(position);
}

///Close PositionDetails Functions
///
/// Closes user position and sends back collateral
///
/// Returns
///  - Profit :The amount to send to position owner
///
/// Note
///  
/// If position order_type is a limit order and not fully filled ,two possibilities exists
///  - If not filled at all ,the collateral is sent back and the debt fully reapid without any interest
///  - If it is partially filled ,the position_type is converted into a market position with the amount filled as the entire position value and the ampount remaining is sent back    

#[ic_cdk::update(name = "closeLimitPosition")]
async fn close_limit_position(account_index: u8) -> Amount {
    let user = ic_cdk::api::msg_caller();

    let account = user._to_subaccount(account_index);

    let mut position = _get_account_position(&account).unwrap();

    let market_details = _get_market_details();

    let vault = Vault::init(market_details.vault_id);
    if let PositionOrderType::Market = position.order_type {
        return 0;
    };

    let (removed_collateral, manage_debt_params) = if position.long {
        _close_limit_long_position(account, &mut position)
    } else {
        _close_limit_short_position(account, &mut position)
    };

    remove_tick_order(position.entry_tick, account);

    if manage_debt_params.amount_repaid != 0 {
        vault.manage_position_update(user, account_index, removed_collateral, manage_debt_params);
    }
    return removed_collateral;
}

#[ic_cdk::update(name = "closeMarketPosition")]
async fn close_market_position(account_index: u8, max_tick: Option<Tick>) -> Amount {
    let user = ic_cdk::api::msg_caller();

    let account = user._to_subaccount(account_index);

    let mut position = _get_account_position(&account).unwrap();

    let market_details = _get_market_details();

    let vault = Vault::init(market_details.vault_id);
    if let PositionOrderType::Market = position.order_type {
        let (collateral_value, crossed_ticks, manage_debt_params) = if position.long {
            _close_market_long_position(account, &mut position, max_tick)
        } else {
            _close_market_short_position(account, &mut position, max_tick)
        };

        _schedule_execution_for_ticks_orders(crossed_ticks);

        if manage_debt_params.amount_repaid != 0 {
            vault.manage_position_update(user, account_index, collateral_value, manage_debt_params);
        }

        return collateral_value;
    } else {
        return 0;
    }
}

/// Liquidate Function
///
/// liquidates an account's position to avoid bad debt by checking if the current leverage exceeds the max leverage
///
/// Note : Position is closed at the current tick
#[ic_cdk::update(name = "liquidatePosition")]
fn liquidate_position(user: Principal, account_index: u8) -> bool {
    let account = user._to_subaccount(account_index);
    let state_details = _get_state_details();

    let market_details = _get_market_details();

    let position =
        _get_account_position(&account).expect("user has no position with this subaccount");

    let (to_liquidate, collateral_remaining, net_debt_value) =
        _liquidation_status(position, state_details.max_leveragex10);

    if to_liquidate {
        let vault = Vault::init(market_details.vault_id);

        let (collateral, amount_repaid) = if collateral_remaining > 0 {
            (collateral_remaining.abs() as u128, net_debt_value)
        } else {
            (0, net_debt_value - (collateral_remaining.abs() as u128))
        };

        let manage_debt_params =
            ManageDebtParams::init(position.debt_value, net_debt_value, amount_repaid);

        _calc_position_realised_value(position.volume_share, position.long);
        vault.manage_position_update(user, account_index, collateral, manage_debt_params);

        _remove_account_position(&account);
        return true;
    }
    return false;
}

async fn _open_position_checks(
    user: Principal,
    account: Subaccount,
    vault: Vault,
    collateral_value: Amount,
    leveragex10: u8,
) -> Result<u32, &'static str> {
    let failed_initial_check = _has_position_or_pending_error_log(&account);

    if failed_initial_check {
        return Err("Account has pending error or unclosed position");
    }

    let StateDetails {
        max_leveragex10,
        min_collateral,
        not_paused,
        ..
    } = _get_state_details();

    if !not_paused {
        return Err("Market is paused");
    }

    // if leverage is greater than max leverage or collateral value is less than min collateral
    //returns
    if leveragex10 >= max_leveragex10 || collateral_value < min_collateral {
        return Err("Max leverage exceeded or collateral is too small");
    }

    // levarage is always given as a multiple of ten
    let debt_value = (u128::from(leveragex10 - 10) * collateral_value) / 10;

    // Checks if user has sufficient balance and vault contains free liquidity greater or equal to debt_value and then calculate interest rate
    let (valid, interest_rate) = vault
        .create_position_validity_check(user, collateral_value, debt_value)
        .await;

    if !valid {
        return Err("Not enough liquidity for debt");
    }
    return Ok(interest_rate);
}

fn _open_limit_short_position(
    _account: Subaccount,
    _collateral_value: Amount,
    _debt_value: Amount,
    _interest_rate: u32,
    _entry_tick: Tick,
) -> Option<(PositionParameters, Vec<Tick>)> {
    let equivalent = |amount: Amount, tick: Tick, buy: bool| -> Amount {
        //  let tick_price = _tick_to_price(tick);
        _equivalent(amount, tick, buy)
    };

    let highest_buy_offer_tick = _get_highest_buy_offer_tick();

    if highest_buy_offer_tick != 0 && _entry_tick <= highest_buy_offer_tick {
        return None;
    }

    let (collateral, debt) = (
        equivalent(_collateral_value, _entry_tick, true),
        equivalent(_debt_value, _entry_tick, true),
    );

    let mut order = LimitOrder::new(collateral + debt, _entry_tick, false);

    _open_order(&mut order);
    let owner = ic_cdk::api::msg_caller();

    let position = PositionParameters {
        owner,
        long: false,
        entry_tick: _entry_tick,
        collateral_value: _collateral_value,
        debt_value: _debt_value,
        interest_rate: _interest_rate,
        volume_share: 0, // not initialised yet
        order_type: PositionOrderType::Limit(order),
        timestamp: 0, //not initialised
    };

    _insert_account_position(_account, position);

    let lowest_sell_offer_tick = _get_lowest_sell_offer_tick();
    if lowest_sell_offer_tick == 0 {
        _update_lowest_sell_offer_tick(_entry_tick);
    } else {
        let active_lowest_sell_offer_tick =
            _get_next_best_offer_tick(true, lowest_sell_offer_tick, _entry_tick);
        if active_lowest_sell_offer_tick.unwrap_or(_entry_tick) == _entry_tick {
            _update_lowest_sell_offer_tick(_entry_tick);
        }
    }

    return Some((position, Vec::new()));
}

fn _open_limit_long_position(
    _account: Subaccount,
    _collateral_value: Amount,
    _debt_value: Amount,
    _interest_rate: u32,
    _entry_tick: Tick,
) -> Option<(PositionParameters, Vec<Tick>)> {
    let lowest_sell_offer_tick = _get_lowest_sell_offer_tick();

    if lowest_sell_offer_tick != 0 && _entry_tick >= lowest_sell_offer_tick {
        return None;
    }

    let (collateral, debt) = (_collateral_value, _debt_value);

    let mut order = LimitOrder::new(collateral + debt, _entry_tick, true);

    _open_order(&mut order);

    let owner = ic_cdk::api::msg_caller();

    let position = PositionParameters {
        owner,
        long: true,
        entry_tick: _entry_tick,
        collateral_value: _collateral_value,
        debt_value: _debt_value,
        interest_rate: _interest_rate,
        volume_share: 0, // not initialised yet
        order_type: PositionOrderType::Limit(order),
        timestamp: 0, //not initialised
    };
    _insert_account_position(_account, position);

    let highest_buy_offer_tick = _get_highest_buy_offer_tick();

    if highest_buy_offer_tick == 0 {
        _update_highest_buy_offer_tick(_entry_tick);
    } else {
        let active_highest_buy_offer_tick =
            _get_next_best_offer_tick(false, highest_buy_offer_tick, _entry_tick);
        if active_highest_buy_offer_tick.unwrap_or(_entry_tick) == _entry_tick {
            _update_highest_buy_offer_tick(_entry_tick);
        }
    }

    return Some((position, Vec::new()));
}

/// Open Market Long Position'
///
/// Params :See Open Position for params definition
fn _open_market_long_position(
    account: Subaccount,
    collateral_value: Amount,
    debt_value: Amount,
    interest_rate: u32,
    max_tick: Option<Tick>,
) -> Option<(PositionParameters, Vec<Tick>)> {
    let (collateral, debt) = (collateral_value, debt_value);

    let lowest_sell_offer_tick = _get_lowest_sell_offer_tick();

    let MarketDetails { tick_spacing, .. } = _get_market_details();

    let stopping_tick = max_tick
        .and_then(|x| Some(_compressed_tick(x, tick_spacing)))
        .unwrap_or(_def_max_tick(lowest_sell_offer_tick, true));

    let (_amount_out, amount_remaining_value, resulting_tick, crossed_ticks) = _swap(
        collateral + debt,
        true,
        lowest_sell_offer_tick,
        stopping_tick,
    );

    let (un_used_debt_value, un_used_collateral_value) = if amount_remaining_value >= debt_value {
        (debt_value, amount_remaining_value - debt_value)
    } else {
        (amount_remaining_value, 0)
    };

    let resulting_debt_value = debt_value - un_used_debt_value;
    let resulting_collateral_value = collateral_value - un_used_collateral_value;

    let position_value = collateral_value + debt_value - amount_remaining_value;

    let volume_share = _calc_position_volume_share(position_value, true);

    let owner = ic_cdk::api::msg_caller();

    let position = PositionParameters {
        owner,
        long: true,
        entry_tick: resulting_tick,
        collateral_value: resulting_collateral_value,
        debt_value: resulting_debt_value, //actual debt
        interest_rate,
        volume_share,
        order_type: PositionOrderType::Market,
        timestamp: ic_cdk::api::time(),
    };

    if resulting_tick > lowest_sell_offer_tick || crossed_ticks.contains(&lowest_sell_offer_tick) {
        let limit = _def_max_tick(resulting_tick, true);
        let nest_best =
            _get_next_best_offer_tick(true, resulting_tick, limit).unwrap_or(resulting_tick);

        _update_lowest_sell_offer_tick(nest_best);
    }
    _insert_account_position(account, position);

    return Some((position, crossed_ticks));
}

/// Open Market Short Position
///
/// Similar to Open Market Long position but for opening short positions
fn _open_market_short_position(
    account: Subaccount,
    collateral_value: Amount,
    debt_value: Amount,
    interest_rate: u32,
    max_tick: Option<Tick>,
) -> Option<(PositionParameters, Vec<Tick>)> {
    let equivalent = |amount: Amount, tick: Tick, buy: bool| -> Amount {
        //  let tick_price = _tick_to_price(tick);
        _equivalent(amount, tick, buy)
    };

    let highest_buy_offer_tick = _get_highest_buy_offer_tick();

    let MarketDetails { tick_spacing, .. } = _get_market_details();
    let stopping_tick = max_tick
        .and_then(|x| Some(_compressed_tick(x, tick_spacing)))
        .unwrap_or(_def_max_tick(highest_buy_offer_tick, false));

    let starting_tick =
        match _get_next_best_offer_tick(false, highest_buy_offer_tick, stopping_tick) {
            Some(tick) => tick,
            None => return None,
        };

    let (collateral, debt) = (
        equivalent(collateral_value, starting_tick, true),
        equivalent(debt_value, starting_tick, true),
    );

    let (amount_out_value, amount_remaining, resulting_tick, crossed_ticks) =
        _swap(collateral + debt, false, starting_tick, stopping_tick);

    let amount_remaining_value = equivalent(amount_remaining, starting_tick, false);

    let (un_used_debt_value, un_used_collateral_value) = if amount_remaining_value >= debt_value {
        (debt_value, amount_remaining_value - debt_value)
    } else {
        (amount_remaining_value, 0)
    };

    let resulting_debt_value = debt_value - un_used_debt_value;
    let resulting_collateral_value = collateral_value - un_used_collateral_value;

    let position_value = amount_out_value;

    let volume_share = _calc_position_volume_share(position_value, false);

    let owner = ic_cdk::api::msg_caller();

    let position = PositionParameters {
        owner,
        long: false,
        entry_tick: resulting_tick,
        collateral_value: resulting_collateral_value,
        debt_value: resulting_debt_value, //actual debt
        interest_rate,
        volume_share,
        order_type: PositionOrderType::Market,
        timestamp: ic_cdk::api::time(),
    };

    if resulting_tick < starting_tick || crossed_ticks.contains(&starting_tick) {
        let limit = _def_max_tick(resulting_tick, false);
        _update_highest_buy_offer_tick(
            _get_next_best_offer_tick(false, resulting_tick, limit).unwrap_or(resulting_tick),
        );
    }
    _insert_account_position(account, position);

    return Some((position, crossed_ticks));
}

/// Close Long PositionDetails
///
///closes a user's  long position if position can be fully closed and  repays debt
///
/// Params
/// - User :The user (position owner)
/// - PositionDetails :The PositionDetails
/// - Current Tick :The current tick of market's state
/// - Stopping Tick : The max tick,corresponds to max price
/// - Vault : Vault canister
///
/// Returns
///  - Current Collateral :The amount to send to position owner after paying debt ,this amount is zero if debt is not fully paid
///  - Resulting Tick :The resulting tick from swapping
///  - Crosssed Ticks :An array of ticks that have been crossed during swapping
///   
/// Note
///  - If position can not be closed fully ,the position is partially closed (updated) and debt is paid back either fully or partially
fn _close_market_long_position(
    account: Subaccount,
    position: &mut PositionParameters,
    max_tick: Option<Tick>,
) -> (Amount, Vec<Tick>, ManageDebtParams) {
    //  let entry_price = _tick_to_price(position.entry_tick);
    let equivalent_at_entry_price =
        |amount: Amount, buy: bool| -> Amount { _equivalent(amount, position.entry_tick, buy) };

    let position_realised_value = _calc_position_realised_value(position.volume_share, true);

    let realised_position_size = equivalent_at_entry_price(position_realised_value, true);

    let highest_buy_offer_tick = _get_highest_buy_offer_tick();

    let MarketDetails { tick_spacing, .. } = _get_market_details();
    let stopping_tick = max_tick
        .and_then(|x| Some(_compressed_tick(x, tick_spacing)))
        .unwrap_or(_def_max_tick(highest_buy_offer_tick, false));

    let Some(starting_tick) =
        _get_next_best_offer_tick(false, highest_buy_offer_tick, stopping_tick)
    else {
        return (
            0,
            Vec::new(),
            ManageDebtParams::init(position.debt_value, position.debt_value, 0),
        );
    };

    let (amount_out_value, amount_remaining, resulting_tick, crossed_ticks) =
        _swap(realised_position_size, false, starting_tick, stopping_tick);

    let interest_value = _calc_interest(
        position.debt_value,
        position.interest_rate,
        position.timestamp,
    );

    let profit: u128;

    let manage_debt_params: ManageDebtParams;

    if amount_remaining > 0 {
        let amount_remaining_value = equivalent_at_entry_price(amount_remaining, false);

        (profit, manage_debt_params) = _update_market_position_after_swap(
            position,
            amount_out_value,
            amount_remaining_value,
            interest_value,
        );

        _insert_account_position(account, position.clone());
    } else {
        let net_debt = position.debt_value + interest_value;
        (profit, manage_debt_params) = (
            amount_out_value - net_debt,
            ManageDebtParams::init(position.debt_value, net_debt, net_debt),
        );
        _remove_account_position(&account);
    }

    if resulting_tick < starting_tick || crossed_ticks.contains(&starting_tick) {
        let limit = _def_max_tick(resulting_tick, false);
        _update_highest_buy_offer_tick(
            _get_next_best_offer_tick(false, resulting_tick, limit).unwrap_or(resulting_tick),
        );
    }
    return (profit, crossed_ticks, manage_debt_params);
}

/// Close Short Position
///
/// similar to Close Long Function,but for short positions
fn _close_market_short_position(
    account: Subaccount,
    position: &mut PositionParameters,
    max_tick: Option<Tick>,
) -> (Amount, Vec<Tick>, ManageDebtParams) {
    let position_realised_value = _calc_position_realised_value(position.volume_share, false);

    let realised_position_size = position_realised_value;

    let lowest_sell_offer_tick = _get_lowest_sell_offer_tick();

    let MarketDetails { tick_spacing, .. } = _get_market_details();

    let stopping_tick = max_tick
        .and_then(|x| Some(_compressed_tick(x, tick_spacing)))
        .unwrap_or(_def_max_tick(lowest_sell_offer_tick, true));

    let Some(starting_tick) =
        _get_next_best_offer_tick(true, lowest_sell_offer_tick, stopping_tick)
    else {
        return (
            0,
            Vec::new(),
            ManageDebtParams::init(position.debt_value, position.debt_value, 0),
        );
    };

    let (amount_out, amount_remaining_value, resulting_tick, crossed_ticks) =
        _swap(realised_position_size, true, starting_tick, stopping_tick);

    let amount_out_value = _equivalent(amount_out, starting_tick, false);

    let interest_value = _calc_interest(
        position.debt_value,
        position.interest_rate,
        position.timestamp,
    );

    let profit: u128;
    let manage_debt_params: ManageDebtParams;

    if amount_remaining_value > 0 {
        (profit, manage_debt_params) = _update_market_position_after_swap(
            position,
            amount_out_value,
            amount_remaining_value,
            interest_value,
        );

        _insert_account_position(account, position.clone());
    } else {
        let net_debt = position.debt_value + interest_value;
        (profit, manage_debt_params) = (
            amount_out_value - net_debt,
            ManageDebtParams::init(position.debt_value, net_debt, net_debt),
        );

        _remove_account_position(&account);
    }

    if resulting_tick > starting_tick || crossed_ticks.contains(&starting_tick) {
        let limit = _def_max_tick(resulting_tick, true);
        _update_lowest_sell_offer_tick(
            _get_next_best_offer_tick(true, resulting_tick, limit).unwrap_or(resulting_tick),
        );
    };

    return (profit, crossed_ticks, manage_debt_params);
}

/// Close Limit Position
///
///
/// Closes a limit position at a particular tick by closing removing the limit order if the order is not filled
///
/// Params
///  - User : The owner of the position
///  - Position : The particular position to close
///  - Vault :The vault type representing the vault canister  
fn _close_limit_long_position(
    account: Subaccount,
    position: &mut PositionParameters,
) -> (Amount, ManageDebtParams) {
    let PositionOrderType::Limit(order) = position.order_type else {
        return (0, ManageDebtParams::default());
    };

    let (amount_received, amount_remaining_value) = _close_order(&order);

    let (removed_collateral, manage_debt_params);

    if amount_received == 0 {
        (removed_collateral, manage_debt_params) = (
            position.collateral_value,
            ManageDebtParams::init(
                position.debt_value,
                position.debt_value,
                position.debt_value,
            ),
        );

        _remove_account_position(&account);
    } else {
        (removed_collateral, manage_debt_params) =
            _convert_limit_position(position, amount_remaining_value);

        _insert_account_position(account, position.clone());
    };

    return (removed_collateral, manage_debt_params);
}

/// Close Limit Short Function
///
/// Similar to close limit long position function but for long position
fn _close_limit_short_position(
    account: Subaccount,
    position: &mut PositionParameters,
) -> (Amount, ManageDebtParams) {
    let PositionOrderType::Limit(order) = position.order_type else {
        // unreachable code
        return (0, ManageDebtParams::default());
    };
    let (amount_received, amount_remaining) = _close_order(&order);

    let (removed_collateral, manage_debt_params);

    if amount_received == 0 {
        (removed_collateral, manage_debt_params) = (
            position.collateral_value,
            ManageDebtParams::init(
                position.debt_value,
                position.debt_value,
                position.debt_value,
            ),
        );
        _remove_account_position(&account);
    } else {
        // let entry_price = _tick_to_price(position.entry_tick);

        let amount_remaining_value = _equivalent(amount_remaining, position.entry_tick, false);
        (removed_collateral, manage_debt_params) =
            _convert_limit_position(position, amount_remaining_value);

        _insert_account_position(account, position.clone());
    };

    return (removed_collateral, manage_debt_params);
}

/// Update Market Position After Swap Function
///
/// This function updates a  market position if it can not be closed i.e amount remaining after swapping to close position is greater than 0
///
/// It
///   - Updates the position debt ,the position collateral value , the position volume share
///   - Derives the update asset params that pays the debt either fully or partially
///  
/// Params
///  - Position :A mutable reference to the particular position
///  - Resulting Tick : The resulting tick after swapping to closing the position
///  - Amount Out Value :The value of the amount gotten from swapping
///  - Amount Remaining Value :The value of the amount remaining after swapping
///  - Interest Value : The value of the interest accrued on current position debt
///
/// Returns
///  - Profit : The amount of profit for position owner or the amount of removable collateral from position
///  - Manage Debt Params : for repaying debt ,specifying the current debt and the previous debt and interest paid
fn _update_market_position_after_swap(
    position: &mut PositionParameters,
    amount_out_value: Amount,
    amount_remaining_value: Amount,
    interest_value: Amount,
) -> (Amount, ManageDebtParams) {
    let init_debt_value = position.debt_value;

    let net_debt_value = init_debt_value + interest_value;

    let profit;
    let manage_debt_params;

    if amount_out_value < net_debt_value {
        position.debt_value = net_debt_value - amount_out_value;

        profit = 0;

        manage_debt_params =
            ManageDebtParams::init(init_debt_value, net_debt_value, amount_out_value);
    } else {
        position.debt_value = 0;
        position.collateral_value = amount_remaining_value;

        profit = amount_out_value - net_debt_value;

        manage_debt_params =
            ManageDebtParams::init(init_debt_value, net_debt_value, net_debt_value);
    }

    let new_volume_share = _calc_position_volume_share(amount_remaining_value, position.long);

    position.volume_share = new_volume_share;

    // if position last time updated is greater than one hour ago ,position time is updated to current timestamp
    if position.timestamp + ONE_HOUR > ic_cdk::api::time() {
        position.timestamp = ic_cdk::api::time()
    }

    return (profit, manage_debt_params);
}

/// Convert Account Limit Position
///
/// Params:
///  - Account :The owner of the position
///  - Read : this flag is utilised if the function is called by a query function and gives a quick return
///   i.e true if called by a read function ,false otherwise
///
/// Returns
///   - is Fully Filled :Returns true  the limit order has been fully filled or returns false otherwise
///   - is Partially Filled :true if the position partially filled
fn _convert_account_limit_position_to_market(account: Subaccount, read: bool) -> PositionStatus {
    let mut position = _get_account_position(&account).unwrap();

    let mut position_status = PositionStatus::FILLED;

    if let PositionOrderType::Limit(order) = position.order_type {
        let (amount_out, amount_remaining) = _close_order(&order);
        if amount_out == 0 {
            position_status = PositionStatus::UNFILLED;
        } else if amount_remaining > 0 {
            position_status = PositionStatus::PARTIAL
        };
        if read {
            return position_status;
        }
        let amount_remaining_value = if position.long {
            amount_remaining
        } else {
            // let price = _tick_to_price(position.entry_tick);
            _equivalent(amount_remaining, position.entry_tick, false)
        };
        _convert_limit_position(&mut position, amount_remaining_value);
        _insert_account_position(account, position);

        // checking if order is completely filled
    }
    return position_status;
}

/// Convert Limit Position function
///
/// Converts a limit position into a market position after the reference limit order of that position has been filled fully or partially
/// any unfilled amount is refunded first as debt and if still remaining it is refunded back to the position owner and the position is updated to a market position
///
/// Params
///  - Position : A mutable reference to the cuurent position
///  - Amount Remaining Value : The value of the amount of  unfilled liquidity of the particular order
///
/// Returns
///  - Removed Collateral : The amount of collateral removed from that position
///  - Update Asset Details Params :The update asset details params for updating asset detailsin params   
fn _convert_limit_position(
    position: &mut PositionParameters,
    amount_remaining_value: Amount,
) -> (Amount, ManageDebtParams) {
    let initial_collateral_value: u128 = position.collateral_value;

    let initial_debt_value = position.debt_value;

    let removed_collateral;
    if amount_remaining_value > initial_debt_value {
        removed_collateral = amount_remaining_value - initial_debt_value;

        position.debt_value = 0;
        position.collateral_value -= removed_collateral;
    } else {
        removed_collateral = 0;

        position.debt_value -= amount_remaining_value;
    }

    let remaining_order_value =
        initial_collateral_value + initial_debt_value - amount_remaining_value;

    let volume_share = _calc_position_volume_share(remaining_order_value, position.long);

    position.volume_share = volume_share;
    position.order_type = PositionOrderType::Market;
    position.timestamp = ic_cdk::api::time();

    let manage_debt_params = ManageDebtParams::init(
        initial_debt_value,
        initial_debt_value,
        initial_debt_value - position.debt_value,
    );

    return (removed_collateral, manage_debt_params);
}

/// Opens Order Functions
///100000100000
/// opens an order at a particular tick
///
/// Params
/// - Order :: a generic type that implements the trait Order for the type of order to close
/// - Reference Tick :: The  tick to place order
fn _open_order(order: &mut LimitOrder) {
    TICKS_DETAILS.with_borrow_mut(|ticks_details| {
        INTEGRAL_BITMAPS.with_borrow_mut(|integrals_bitmaps| {
            let mut open_order_params = OpenOrderParams {
                order,
                integrals_bitmaps,
                ticks_details,
            };
            open_order_params.open_order();
        })
    });
}
/// Close Order Function
///
/// closes an order at a particular tick
///
/// Params :
///  - Order :: a generic that implements the trait Order for the type of order to close
///  - Order Size :: Tha amount of asset in order
///  - Order Direction :: Either a buy or a sell
///  - Order Reference Tick :: The tick where order was placed  
///
/// Returns
///  - Amont Out :: This corresponds to the asset to be bought i.e perp(base) asset for a buy order or quote asset for a sell order
///  - Amount Remaining :: This amount remaining corrseponds to the amount of asset at that tick that is still unfilled
fn _close_order(order: &LimitOrder) -> (Amount, Amount) {
    TICKS_DETAILS.with_borrow_mut(|ticks_details| {
        INTEGRAL_BITMAPS.with_borrow_mut(|integrals_bitmaps| {
            let mut close_order_params = CloseOrderParams {
                order,
                integrals_bitmaps,
                ticks_details,
            };
            close_order_params.close_order()
        })
    })
}

/// Swap Function
///
/// Params
///  - Order Size :: Tha amount of asset in order
///  - Buy :: the order direction ,true for buy and false otherwise
///  - Init Tick :: The current state tick
///  - Stopping Tick :: The maximum tick ,corresponds to maximum price
///
/// Returns
///  - Amount Out :: The amount out froom swapping
///  - Amount Remaining :: The amount remaining from swapping
///  - resulting Tick :The last tick at which swap occured
///  - Crossed Ticks :: An vector of all ticks crossed during swap
fn _swap(
    order_size: Amount,
    buy: bool,
    init_tick: Tick,
    stopping_tick: Tick,
) -> (Amount, Amount, Tick, Vec<Tick>) {
    TICKS_DETAILS.with_borrow_mut(|ticks_details| {
        INTEGRAL_BITMAPS.with_borrow_mut(|integrals_bitmaps| {
            let mut swap_params = SwapParams {
                buy,
                init_tick,
                stopping_tick,
                order_size,
                integrals_bitmaps,
                ticks_details,
            };
            swap_params._swap()
        })
    })
}

/// Calculate Position PNL
///
/// Calculates the current pnl in percentage  for a particular position
///
/// Returns
///  PNL :The pnl(in percentage) on that position
///  Net Debt Value :The net debt on that position
fn _calculate_position_unrealised_pnl_and_net_debt_value(
    position: PositionParameters,
) -> (i64, Amount) {
    let interest_on_debt_value = _calc_interest(
        position.debt_value,
        position.interest_rate,
        position.timestamp,
    );

    let net_debt_value = position.debt_value + interest_on_debt_value;

    let current_tick = _get_lowest_sell_offer_tick() as i64;
    // let current_tick = _tick_to_price(current_tick) as i64;
    //let entry_price = _tick_to_price(position.entry_tick) as i64;
    let entry_tick = position.entry_tick as i64;

    let pnl: i64;
    if position.long {
        pnl = ((current_tick - entry_tick) * 100 * (_ONE_PERCENT as i64)) / entry_tick
    } else {
        pnl = ((entry_tick - current_tick) * 100 * (_ONE_PERCENT as i64)) / entry_tick
    }
    return (pnl, net_debt_value);
}

/// Liquidation Status Function
///
/// Checks if a position is to be liquidated and the corrseponding collateral for liquidating that position
///
/// Params ;
///  - Position :The Position to check
///  - Max Leverage :The current maximum leverage for opening a position
///
/// Returns
///  - To Liquidate :true if position should be liquidated
///  - Collateral_Remaining : Returns the current value of collateral within the position
///  - Net Debt Value :returns the net debt value
///
/// Note :This collateral value can be less than zero in such case, a bad debt has occured
fn _liquidation_status(position: PositionParameters, max_leveragex10: u8) -> (bool, i128, Amount) {
    if let PositionOrderType::Market = position.order_type {
        let initial_position_value = position.collateral_value + position.debt_value;

        let (pnl_in_percentage, net_debt_value) =
            _calculate_position_unrealised_pnl_and_net_debt_value(position);

        let position_profit_or_loss =
            _percentage(pnl_in_percentage.abs() as u64, initial_position_value);

        let current_collateral_value = if pnl_in_percentage >= 0 {
            (initial_position_value + position_profit_or_loss) as i128 - (net_debt_value) as i128
        } else {
            (initial_position_value as i128) - (position_profit_or_loss + net_debt_value) as i128
        };

        let current_leverage_x10 =
            ((net_debt_value as i128 + current_collateral_value) * 10) / current_collateral_value;

        let to_liquidate =
            current_collateral_value < 0 || current_leverage_x10.abs() as u8 >= max_leveragex10;

        return (to_liquidate, current_collateral_value, net_debt_value);
    }

    return (false, position.collateral_value as i128, 0);
}

/// Get  Next Best Offer Tick
///
/// Gets the best tick i.e best price to buy or sell from the checking  from the current tick to the max tick   
///
///  Params
///  - Direction :the swap direction ,true for a sell and false for a buy
///  - Starting Tick :The starting tick to check for liquidity/// Get  Best Offer Tick
///
/// Gets the best tick i.e best price to buy or sell from the checking  from the current tick to the max tick   
///
///  Params
///  - Direction :the swap direction ,true for a sell and false for a buy
///  - Starting Tick :The starting tick to check for liquidity
///  - Stopping Tick :The max tick to stopping checking at

fn _get_next_best_offer_tick(qforb: bool, starting_tick: Tick, max_tick: Tick) -> Option<Tick> {
    TICKS_DETAILS.with_borrow_mut(|ticks_details| {
        INTEGRAL_BITMAPS.with_borrow_mut(|integrals_bitmaps| {
            _get_best_offer(
                qforb,
                starting_tick,
                max_tick,
                integrals_bitmaps,
                ticks_details,
            )
        })
    })
}

fn _update_highest_buy_offer_tick(next_tick: Tick) {
    HIGHEST_BUY_OFFER.with_borrow_mut(|tick| *tick = next_tick)
}

fn _update_lowest_sell_offer_tick(next_tick: Tick) {
    LOWEST_SELL_OFFER.with_borrow_mut(|tick| *tick = next_tick)
}

fn _get_highest_buy_offer_tick() -> Tick {
    HIGHEST_BUY_OFFER.with_borrow(|tick| tick.clone())
}

fn _get_lowest_sell_offer_tick() -> Tick {
    LOWEST_SELL_OFFER.with_borrow(|tick| tick.clone())
}

///////////////////////////////////////////////////////////////////////////////////////////////
//////////////////////////////////////////////////////////////////////////////////////////////
///  Funding Rate Functions
///////////////////////////////////////////////////////////////////////////////////////////////
///////////////////////////////////////////////////////////////////////////////////////////////
/// Settle Funcding Rate
///
/// Settles Funding Rate by calling the XRC cansiter .fetching the Price ,calculating the premium and distributing the  fund to the right market direction,Long or Short
async fn settle_funding_rate() {
    let market_details = _get_market_details();

    let xrc = XRC::init(market_details.xrc_id);

    let request = GetExchangeRateRequest {
        base_asset: market_details.base_asset,
        quote_asset: market_details.quote_asset,
        timestamp: None,
    };

    match xrc._get_exchange_rate(request).await {
        Ok(rate_result) => {
            let current_price_tick = _get_lowest_sell_offer_tick() as u128;

            let perp_price =
                (current_price_tick * 10u128.pow(rate_result.metadata.decimals)) / _PRICE_FACTOR;

            let spot_price = rate_result.rate as u128;

            _settle_funding_rate(perp_price, spot_price);
        }
        Err(_) => {
            return;
        }
    }
}

fn _settle_funding_rate(perp_price: u128, spot_price: u128) {
    let funding_rate = _calculate_funding_rate_premium(perp_price, spot_price);
    FUNDING_RATE_TRACKER.with_borrow_mut(|reference| {
        let mut funding_rate_tracker = reference.get().clone();

        funding_rate_tracker.settle_funding_rate(funding_rate.abs() as u64, funding_rate > 0);

        reference.set(funding_rate_tracker).unwrap();
    })
}

fn _calculate_funding_rate_premium(perp_price: u128, spot_price: u128) -> i64 {
    let funding_rate = ((perp_price as i128 - spot_price as i128) * 100 * _ONE_PERCENT as i128)
        / spot_price as i128;
    return funding_rate as i64;
}
///Calculate Position Realised value
///
///Calculates the Realised value for a position's volume share in a particular market direction,Long or Short   
///
/// Note:This function also adjust's the volume share
fn _calc_position_realised_value(volume_share: Amount, long: bool) -> Amount {
    FUNDING_RATE_TRACKER.with_borrow_mut(|tr| {
        let mut funding_rate_tracker = tr.get().clone();

        let value = funding_rate_tracker.remove_volume(volume_share, long);

        tr.set(funding_rate_tracker).unwrap();
        value
    })
}
/// Calculate Position Volume Share
///
/// Calculates the volume share for a particular poistion volume in a market direction ,Long or Short
fn _calc_position_volume_share(position_value: Amount, long: bool) -> Amount {
    FUNDING_RATE_TRACKER.with_borrow_mut(|tr| {
        let mut funding_rate_tracker = tr.get().clone();

        let value = funding_rate_tracker.add_volume(position_value, long);

        tr.set(funding_rate_tracker).unwrap();
        value
    })
}
////////////////////////////////////////////////////////////////////////////////////////////////
///////////////////////////////////////////////////////////////////////////////////////////////
////////////////////////////////////////////////////////////////////////////////////////////////

//////////////////////////////////////////////////////////////////////////////////////////////////
////////////////////////////////////////////////////////////////////////////////////////////////
/// # Limit Order Functions
///
/// These function serves as a mechanism for handling limit orders that reference a particular tick that has been crossed due to a market order crossing a price point
///
/// Operation
///   - each account with a limit order is stored in an array that is mapped to the reference tick of that order in a hashmap (LIMIT ORDER RECORD L.O.R ),
///  when a limmit order is placed at any tick the account opening the position  is pushed added to  the array mapped to the tick in LOR
/// similarly if a limit order is cancelled fully (unfilled) or fored closed (partially filled) the account of that order is removed from the array mapped to tick in LOR
///
///   - When a market order crosses a tick ,it is assumed that all limit orders at that tick has been completely filled so they are closed  by first calling the
/// ```
///    fn _schedule_execution_for_ticks_orders(crossed_ticks: Vec<Tick>)
/// /// passing in an array of all the ticks crossed during the swap  
///
/// /// This function serializes all accounts within ticks that have been crossed creates a one time timer that calls the execute each limit other function
///    pub fn _execute_account_serialization_for_each_tick(ticks: Vec<Tick>)
/// /// This function retrieves each account from the
///
/// fn _execute_each_limit_order()
///  /// The function gets the array mapped to a pariicular tick  and add each element
/// ```
//

pub fn store_tick_order(tick: Tick, account: Subaccount) {
    LIMIT_ORDERS_RECORD.with_borrow_mut(|reference| {
        let accounts = reference.entry(tick).or_insert(Vec::new());

        accounts.push(account);
    })
}

/// Remove Tick Order
///
/// Removes an order under a particular tick
///
/// Utilised when account owner closes a limit position before reference tick is fully crossed
///
/// Params
/// - Tick    :The tickat which order was placed
/// - Account : The account closing the position
pub fn remove_tick_order(tick: Tick, account: Subaccount) {
    LIMIT_ORDERS_RECORD.with_borrow_mut(|reference| {
        let accounts = reference.get_mut(&tick).unwrap();

        let index = accounts.iter().position(|x| x == &account).unwrap();

        accounts.remove(index);
    })
}

/// Schedule Execution For Ticks Orders
///
/// Utilised for scheduling the execution of ticks order by calling the _execute_ticks_orders  function after some seconds
fn _schedule_execution_for_ticks_orders(crossed_ticks: Vec<Tick>) {
    if crossed_ticks.len() == 0 {
        return;
    }

    _execute_accounts_serialization_for_each_tick(crossed_ticks)
}

/// Execute Ticks Orders
///
/// Ticks:  An array of ticks crossed during the swap (meaning all orders at those tick has been filled)
pub fn _execute_accounts_serialization_for_each_tick(ticks: Vec<Tick>) {
    EXECUTABLE_LIMIT_ORDERS_ACCOUNTS.with_borrow_mut(|accounts| {
        LIMIT_ORDERS_RECORD.with_borrow_mut(|reference| {
            for tick in ticks {
                let mut tick_accounts = reference.get_mut(&tick).unwrap();
                //  let acc: Vec<Subaccount> = accounts.   .try_into().unwrap();
                accounts.append(&mut tick_accounts);
            }
        });
    });

    let pending_timer = _get_pending_timer();

    if pending_timer == TimerId::default() {
        let timer_id =
            ic_cdk_timers::set_timer_interval(Duration::from_nanos(6 * ONE_SECOND), || {
                _execute_each_limit_order();
            });

        _set_pending_timer(timer_id);
    }
}

/// Execute Each Limit Order
///
fn _execute_each_limit_order() {
    EXECUTABLE_LIMIT_ORDERS_ACCOUNTS.with_borrow_mut(|reference| {
        if let Some(account) = reference.pop() {
            _convert_account_limit_position_to_market(account, false);
        } else {
            let timer_id = _get_pending_timer();

            ic_cdk_timers::clear_timer(timer_id);
            _set_pending_timer(TimerId::default());
        }
    })
}
//////////////////////////////////////////////////////////////////////////////////////////////
/////////////////////////////////////////////////////////////////////////////////////////////

//////////////////////////////////////////////////////////////////////////////////////////////
/////////////////////////////////////////////////////////////////////////////////////////////
/// Admin Functions
//////////////////////////////////////////////////////////////////////////////////////////////
//////////////////////////////////////////////////////////////////////////////////////////////
fn admin_guard() -> Result<(), String> {
    ADMIN.with_borrow(|admin_ref| {
        let admin = admin_ref.get().clone();
        if ic_cdk::api::msg_caller() == admin {
            return Ok(());
        } else {
            return Err("Invalid".to_string());
        };
    })
}

#[ic_cdk::update(guard = "admin_guard", name = "updateStateDetails")]
async fn update_state_details(new_state_details: StateDetails) {
    _set_state_details(new_state_details);
}

#[ic_cdk::update(guard = "admin_guard", name = "startTimer")]
async fn start_timer() {
    ic_cdk_timers::set_timer_interval(Duration::from_nanos(ONE_HOUR), || {
        ic_cdk::futures::spawn(settle_funding_rate())
    });
}

//////////////////////////////////////////////////////////////////////////////////////////////////
//////////////////////////////////////////////////////////////////////////////////////////////////
////////////////////////////////////////////////////////////////////////////////////////////////////
///  Error Handling Functions
//////////////////////////////////////////////////////////////////////////////////////////////////
/// ///////////////////////////////////////////////////////////////////////////////////////////////
////////////////////////////////////////////////////////////////////////////////////////////////////
fn trusted_canister_guard() -> Result<(), String> {
    let market_details = _get_market_details();

    let caller = ic_cdk::api::msg_caller();

    if caller == market_details.vault_id {
        return Ok(());
    } else {
        return Err("Untrusted Caller".to_string());
    }
}

#[ic_cdk::update(name = "retryAccountError")]
async fn retry_account_error(_user_account: Subaccount) {
    let account_error_log = _get_account_error_log(&_user_account);

    let details = _get_market_details();
    account_error_log.retry(details);
}

#[ic_cdk::update(name = "successNotification", guard = "trusted_canister_guard")]
fn success_notif(account: Subaccount, _error_index: usize) {
    let market_details = _get_market_details();

    let caller = ic_cdk::api::msg_caller();

    if caller == market_details.vault_id {
        _remove_account_error_log(&account);
        return;
    }
}
/////////////////////////////////////////////////////////////////////////////////////////////////////
/////////////////////////////////////////////////////////////////////////////////////////////////////
/////////////////////////////////////////////////////////////////////////////////////////////////////

///////////////////////////////////////////////////////////////////////////////////////////////////////
/////////////////////////////////////////////////////////////////////////////////////////////////////
/// Getter Functions
///////////////////////////////////////////////////////////////////////////////////////////////////////

fn _get_market_details() -> MarketDetails {
    MARKET_DETAILS.with_borrow(|ref_market_details| ref_market_details.get().clone())
}

fn _get_state_details() -> StateDetails {
    STATE_DETAILS.with_borrow(|ref_state_detaills| *ref_state_detaills.get())
}

fn _get_account_position(account: &Subaccount) -> Option<PositionParameters> {
    ACCOUNTS_POSITION.with_borrow(|ref_position_details| ref_position_details.get(&account))
}

fn _get_account_error_log(account: &Subaccount) -> PositionUpdateErrorLog {
    ACCOUNTS_ERROR_LOGS.with_borrow(|reference| reference.get(account).unwrap())
}

fn _get_tick_details(tick: Tick) -> Option<TickDetails> {
    TICKS_DETAILS.with_borrow(|ref_tick_details| ref_tick_details.get(&tick))
}

fn _get_pending_timer() -> TimerId {
    PENDING_TIMER.with_borrow(|reference| reference.clone())
}

fn _has_position_or_pending_error_log(_account: &Subaccount) -> bool {
    let has_position = ACCOUNTS_POSITION.with_borrow(|reference| reference.contains_key(_account));
    let has_pending_error =
        ACCOUNTS_ERROR_LOGS.with_borrow(|reference| reference.contains_key(_account));

    return has_pending_error || has_position;
}

////////////////////////////////////////////////////////////////////////////////////////////////////
/// ////////////////////////////////////////////////////////////////////////////////////////////////
/////////////////////////////////////////////////////////////////////////////////////////////////////

///////////////////////////////////////////////////////////////////////////////////////////////////
/// /////////////////////////////////////////////////////////////////////////////////////////////////
///   Setter Function
//////////////////////////////////////////////////////////////////////////////////////////////////////
fn _set_state_details(new_state: StateDetails) {
    STATE_DETAILS.with_borrow_mut(|ref_state_details| ref_state_details.set(new_state).unwrap());
}

fn _insert_account_position(account: Subaccount, position: PositionParameters) {
    ACCOUNTS_POSITION
        .with_borrow_mut(|ref_users_position| ref_users_position.insert(account, position));
}

fn _remove_account_position(account: &Subaccount) {
    ACCOUNTS_POSITION.with_borrow_mut(|ref_user_position| ref_user_position.remove(account));
}

fn _insert_account_error_log(account: Subaccount, error_log: PositionUpdateErrorLog) {
    ACCOUNTS_ERROR_LOGS.with_borrow_mut(|reference| reference.insert(account, error_log));
}

fn _remove_account_error_log(account: &Subaccount) {
    ACCOUNTS_ERROR_LOGS.with_borrow_mut(|reference| reference.remove(account));
}

fn _set_pending_timer(timer_id: TimerId) {
    PENDING_TIMER.with_borrow_mut(|reference| {
        *reference = timer_id;
    })
}
////////////////////////////////////////////////////////////////////////////////////////////////////
///////////////////////////////////////////////////////////////////////////////////////////////////
////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(CandidType, Deserialize, PartialEq, Eq, Debug, Serialize, Clone, Copy)]
enum OrderType {
    Market,
    Limit,
}

#[derive(CandidType, Deserialize, Debug, Serialize, Clone, Copy)]
enum PositionOrderType {
    Market,
    Limit(LimitOrder),
}

#[derive(CandidType, Deserialize, Serialize, Debug, Clone, Copy)]
struct PositionParameters {
    owner: Principal,
    /// Entry Tick
    ///
    /// The tick at which position is opened
    entry_tick: Tick,
    /// true if long
    long: bool,
    /// Collatreal Value
    ///
    /// collatreal within position
    collateral_value: Amount,
    /// Debt
    ///
    /// the amount borrowed as leveragex10
    ///
    /// Note:debt is in perp Asset when shorting and in collateral_value asset when longing
    debt_value: Amount,
    // /// PositionDetails Size
    // ///
    // /// The amount of asset in position
    // ///
    // /// This can either be
    // ///
    // ///  - The amount resulting from the _swap when opening a position or
    // ///  - The amount used to gotten from opening placing order at a tick in the case of an order type
    // position_size: Amount,
    /// Volume Share
    ///
    ///Measure of liqudiity share in position with respect to the net amount in all open position of same direction i.e
    /// LONG or SHORT
    volume_share: Amount,
    /// Intrerest Rate
    ///
    /// Current interest rate for opening a position with margin
    ///
    interest_rate: u32,
    ///Order Type
    ///
    ///Position Order  type can either be a
    ///
    /// Market
    ///  - This is when position is opened instantly at the current price
    ///
    /// Order   let _user_account = user._to_subaccount();
    ///   - This comprises of an order set at a particular tick and position is only opened when
    ///   that  order has been executed
    order_type: PositionOrderType,

    /// TimeStamp
    ///
    /// timestamp when psotion was executed opened
    /// Tnis corresponds to the start time for  calculating interest rate on a leveraged position
    ///
    /// Note: For order type, position this  is time  order was excuted
    timestamp: Time,
}

impl Storable for PositionParameters {
    const BOUND: Bound = Bound::Bounded {
        max_size: 180,
        is_fixed_size: false,
    };
    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        // Direct binary deserialization using bincode for better performance
        bincode::deserialize(bytes.as_ref()).expect("Failed to deserialize TickDetails")
    }

    fn to_bytes(&self) -> Cow<[u8]> {
        // Direct binary serialization using bincode for better performance
        let serialized = bincode::serialize(self).expect("Failed to serialize TickDetails");
        Cow::Owned(serialized)
    }
}

#[derive(CandidType)]
enum PositionStatus {
    FILLED,
    UNFILLED,
    PARTIAL,
}
/// ManageDebtParams is utilised to handle debt handling and  repayment
#[derive(Copy, Clone, Default, Deserialize, CandidType)]
struct ManageDebtParams {
    initial_debt: Amount,
    net_debt: Amount,
    amount_repaid: Amount,
}

impl ManageDebtParams {
    fn init(initial_debt: Amount, net_debt: Amount, amount_repaid: Amount) -> Self {
        ManageDebtParams {
            initial_debt,
            net_debt,
            amount_repaid,
        }
    }
}

/////////////////////////////
///   Possible error during inter canister calls and retry api
////////////////////////////

/// Retrying Trait
///
/// Trait for all Errors related to inter canister calls
trait Retrying {
    /// Retry  Function
    ///
    /// This is used to retry the  failed inter canister call
    fn retry(&self, details: MarketDetails);
}

/// ManageDebtError
///
/// This error occurs for failed intercanister calls
#[derive(Clone, Copy, Deserialize, CandidType)]
struct PositionUpdateErrorLog {
    user: Principal,
    profit: Amount,
    debt_params: ManageDebtParams,
}
impl Retrying for PositionUpdateErrorLog {
    fn retry(&self, details: MarketDetails) {
        let call = Call::bounded_wait(details.vault_id, "managePositionUpdate").with_args(&(
            self.user,
            self.profit,
            self.debt_params,
        ));
        if let Ok(()) = call.oneway() {
            return;
        };
    }
}

impl Storable for PositionUpdateErrorLog {
    const BOUND: Bound = Bound::Bounded {
        max_size: 100,
        is_fixed_size: false,
    };
    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        Decode!(bytes.as_ref(), Self).unwrap()
    }

    fn to_bytes(&self) -> Cow<[u8]> {
        Cow::Owned(Encode!(self).unwrap())
    }
}

/// Exchange Rate Canister
///
/// Utilised for fetching the price of current exchnage rate (spot price) of the market pair
struct XRC {
    canister_id: Principal,
}

impl XRC {
    fn init(canister_id: Principal) -> Self {
        XRC { canister_id }
    }

    /// tries to fetch the current exchange rate of the pair and returns the result
    async fn _get_exchange_rate(&self, request: GetExchangeRateRequest) -> GetExchangeRateResult {
        let call = Call::unbounded_wait(self.canister_id, "get_exchange_rate")
            .with_arg(request)
            .with_cycles(1_000_000_000);

        return call.await.unwrap().candid().unwrap();
    }
}

/// The Vault type representing vault canister that stores asset for the entire collateral's denominated market
/// it facilitates all movement of assets including collection and repayment of debt utilised for leverage
#[derive(Clone, Copy)]
struct Vault {
    canister_id: Principal,
}

impl Vault {
    // initialises the vault canister
    pub fn init(canister_id: Principal) -> Self {
        Vault { canister_id }
    }

    /// Manage Position Update
    ///
    /// Utilised when position is updated or closed
    /// Utilised when for updating user_balance,repayment of debt
    pub fn manage_position_update(
        &self,
        user: Principal,
        account_index: u8,
        profit: Amount,
        manage_debt_params: ManageDebtParams,
    ) {
        let call = Call::bounded_wait(self.canister_id, "managePositionUpdate").with_args(&(
            user,
            profit,
            manage_debt_params,
        ));
        if let Ok(()) = call.oneway() {
        } else {
            let error_log = PositionUpdateErrorLog {
                user,
                profit,
                debt_params: manage_debt_params,
            };
            _insert_account_error_log(user._to_subaccount(account_index), error_log);
        }
    }

    /// Create Position Validity Check
    ///
    /// Checks if position can be opened by checking that uswer has sufficient balance and amount to use as debt is available as free liquidity
    ///
    /// User:The Owner of Account that opened position
    /// Collateral Delta:The Amount of asset used as collateral for opening position
    /// Debt : The Amount of asset taken as debt
    ///
    /// Note :After checking that the condition holds ,the user balance is reduced by collateral amount and the free liquidity available is reduced by debt amount

    pub async fn create_position_validity_check(
        &self,
        user: Principal,
        collateral: Amount,
        debt: Amount,
    ) -> (bool, u32) {
        // return (true, 0);
        let call = Call::bounded_wait(self.canister_id, "createPositionValidityCheck")
            .with_args(&(user, collateral, debt));

        if let Ok(response) = call.await {
            return response.candid_tuple().unwrap();
        } else {
            return (false, 0);
        }
    }
}

trait UniqueSubAccount {
    fn _to_subaccount(&self, index: u8) -> Subaccount;
}

impl UniqueSubAccount for Principal {
    fn _to_subaccount(&self, index: u8) -> Subaccount {
        let mut hasher = Sha256::new();
        hasher.update(self.as_slice());
        hasher.update(&(index).to_be_bytes());
        let hash = hasher.finalize();
        let mut subaccount = [0u8; 32];
        subaccount.copy_from_slice(&hash[..32]);
        subaccount
    }
}

export_candid!();

pub mod corelib;
pub mod types;

#[cfg(test)]
pub mod closed_integration_tests;
