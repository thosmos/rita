//! This binary is run to have access to a postgresql db, takes its client info and writes it into
//! smart contract. This is necessary setup to move existing registered clients from the previous
//! sql db format to a smart contract

#![warn(clippy::all)]
#![allow(clippy::pedantic)]
#![forbid(unsafe_code)]

use std::{thread, time::Duration};

use clarity::PrivateKey;
use docopt::Docopt;
use log::{error, info};
use rita_client_registration::register_client_batch_loop::register_client_batch_loop;
use rita_db_migration::start_db_migration;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Args {
    pub flag_dburl: String,
    pub flag_address: String,
    pub flag_web3url: String,
    pub flag_privatekey: String,
}

#[actix_rt::main]
async fn main() {
    env_logger::Builder::default()
        .filter(None, log::LevelFilter::Info)
        .init();

    let args: Args = Docopt::new(get_arg_usage())
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    info!("Received Args: {:?}", args);

    let db_url = args.flag_dburl;
    let contract_addr = args
        .flag_address
        .parse()
        .expect("Please provide a valid eth contract addr");
    let private_key: PrivateKey = args
        .flag_privatekey
        .parse()
        .expect("Please provide a valid eth private key with funds");

    info!("About to add user admin");

    thread::sleep(Duration::from_secs(5));

    info!("About to add start registration loop");
    // Start registration loop
    register_client_batch_loop(args.flag_web3url.clone(), contract_addr, private_key);

    thread::sleep(Duration::from_secs(3));

    info!("About to start db migration loop");
    match start_db_migration(
        db_url,
        args.flag_web3url,
        private_key.to_address(),
        contract_addr,
    )
    .await
    {
        Ok(_) => info!(
            "Successfully queued all clients for migration! Close the program once this is done"
        ),
        Err(e) => error!("Failed to migrate clients with {}", e),
    }

    thread::sleep(Duration::from_secs(60000));
}

pub fn get_arg_usage() -> String {
    "Usage: 
    rita_db_migration --dburl=<dburl> --address=<address> --web3url=<web3url> --privatekey=<privatekey>

Options:
    -u, --dburl=<dburl>           Postgresql db url
    -a, --address=<address>         Smart Contract address
    -w, --web3url=<web3url>       Web3 url
    -p, --privatekey=<privatekey>     The contract state admin private key

About: 
    Db migration binary".to_string()
}
