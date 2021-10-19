use crate::blockchain_oracle::zero_window_start;
use crate::rita_loop::get_web3_server;
use crate::token_bridge::setup_withdraw as bridge_withdraw;
use crate::token_bridge::Withdraw as WithdrawMsg;
use actix_web::http::StatusCode;
use actix_web::HttpResponse;
use actix_web::Path;
use althea_types::SystemChain;
use clarity::{Address, Transaction};
use failure::Error;
use futures01::{future, Future};
use num256::Uint256;

use std::boxed::Box;
use std::time::Duration;
use web30::client::Web3;

pub const WITHDRAW_TIMEOUT: Duration = Duration::from_secs(10);

fn withdraw_handler(
    address: Address,
    amount: Option<Uint256>,
) -> Box<dyn Future<Item = HttpResponse, Error = Error>> {
    debug!("/withdraw/{:#x}/{:?} hit", address, amount);
    let payment_settings = settings::get_rita_common().payment;
    let system_chain = payment_settings.system_chain;
    let withdraw_chain = payment_settings.withdraw_chain;
    let mut gas_price = payment_settings.gas_price.clone();
    let balance = payment_settings.balance;

    // if no amount is specified we are withdrawing our entire balance
    let mut amount = if let Some(amount) = amount {
        amount
    } else {
        balance.clone()
    };

    let tx_gas: Uint256 =
        if (system_chain, withdraw_chain) == (SystemChain::Xdai, SystemChain::Ethereum) {
            // this is the hardcoded gas price over in token bridge so we have to use it
            gas_price = 10_000_000_000u128.into();
            // this is a contract call
            80000u32.into()
        } else {
            21000u32.into()
        };

    let tx_cost = gas_price * tx_gas;
    if amount.clone() + tx_cost.clone() >= balance {
        zero_window_start();
        amount = balance - tx_cost;
    }

    match (system_chain, withdraw_chain) {
        (SystemChain::Ethereum, SystemChain::Ethereum) => eth_compatable_withdraw(address, amount),
        (SystemChain::Rinkeby, SystemChain::Rinkeby) => eth_compatable_withdraw(address, amount),
        (SystemChain::Xdai, SystemChain::Xdai) => eth_compatable_withdraw(address, amount),
        (SystemChain::Xdai, SystemChain::Ethereum) => xdai_withdraw(address, amount),
        (_, _) => Box::new(future::ok(
            HttpResponse::new(StatusCode::from_u16(500u16).unwrap())
                .into_builder()
                .json(format!(
                    "System chain is {} but withdraw chain is {}, withdraw impossible!",
                    system_chain, withdraw_chain
                )),
        )),
    }
}

pub fn withdraw(
    path: Path<(Address, Uint256)>,
) -> Box<dyn Future<Item = HttpResponse, Error = Error>> {
    withdraw_handler(path.0, Some(path.1.clone()))
}

pub fn withdraw_all(path: Path<Address>) -> Box<dyn Future<Item = HttpResponse, Error = Error>> {
    let address = path.into_inner();
    debug!("/withdraw_all/{} hit", address);
    withdraw_handler(address, None)
}

/// Withdraw for eth compatible chains
fn eth_compatable_withdraw(
    address: Address,
    amount: Uint256,
) -> Box<dyn Future<Item = HttpResponse, Error = Error>> {
    let full_node = get_web3_server();
    let web3 = Web3::new(&full_node, WITHDRAW_TIMEOUT);
    let payment_settings = settings::get_rita_common().payment;
    if payment_settings.eth_address.is_none() {
        return Box::new(future::ok(
            HttpResponse::new(StatusCode::from_u16(504u16).unwrap())
                .into_builder()
                .json("No Address configured, withdraw impossible!"),
        ));
    };

    let tx = Transaction {
        nonce: payment_settings.nonce.clone(),
        gas_price: payment_settings.gas_price.clone(),
        gas_limit: 21_000u32.into(),
        to: address,
        value: amount,
        data: Vec::new(),
        signature: None,
    };
    let transaction_signed = tx.sign(
        &payment_settings
            .eth_private_key
            .expect("No private key configured!"),
        payment_settings.net_version,
    );

    let transaction_bytes = match transaction_signed.to_bytes() {
        Ok(bytes) => bytes,
        Err(e) => {
            return Box::new(future::ok(
                HttpResponse::new(StatusCode::from_u16(500u16).unwrap())
                    .into_builder()
                    .json(format!("Transaction to bytes failed! {:?}", e)),
            ));
        }
    };

    let transaction_status = web3.eth_send_raw_transaction(transaction_bytes);

    Box::new(transaction_status.then(move |result| match result {
        Ok(tx_id) => Box::new(future::ok({
            let mut common = settings::get_rita_common();

            common.payment.nonce += 1u64.into();

            settings::set_rita_common(common);
            HttpResponse::Ok().json(format!("txid:{:#066x}", tx_id))
        })),
        Err(e) => {
            if e.to_string().contains("nonce") {
                Box::new(future::ok(
                    HttpResponse::new(StatusCode::from_u16(500u16).unwrap())
                        .into_builder()
                        .json(format!("The nonce was not updated, try again {:?}", e)),
                ))
            } else {
                Box::new(future::ok(
                    HttpResponse::new(StatusCode::from_u16(500u16).unwrap())
                        .into_builder()
                        .json(format!("Full node failed to send transaction! {:?}", e)),
                ))
            }
        }
    }))
}

/// Cross chain bridge withdraw from Xdai -> ETH
/// This handler invokes a withdraw function that sets a bool (as a lock) and withdraw information
/// as a lazy static. This is done in a sync context since our handler uses the older version of
/// futures. From there our xdai_loop ticks, looks at the lazy static for updated information and
/// sends out a transaction to the contract 'relayTokens' on xdai blockchain, that sends the funds
/// directly to an external address without eth conversion. This can be done in the async context
/// using new futures. From there we constantly check the blockchain for any withdrawal events.
/// We send these events as a contract call to simulate them, and those that do succeed, we execute
/// to unlock the funds on eth side.
fn xdai_withdraw(
    address: Address,
    amount: Uint256,
) -> Box<dyn Future<Item = HttpResponse, Error = Error>> {
    Box::new(
        match bridge_withdraw(WithdrawMsg {
            to: address,
            amount,
        }) {
            Ok(_) => Box::new(future::ok(
                HttpResponse::Ok().json("View endpoints for progress"),
            )),
            Err(e) => Box::new(future::ok(
                HttpResponse::new(StatusCode::from_u16(500u16).unwrap())
                    .into_builder()
                    .json(format!("{:?}", e)),
            )),
        },
    )
}