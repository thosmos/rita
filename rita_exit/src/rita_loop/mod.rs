//! This is the primary actor loop for rita-exit, where periodic tasks are spawned and Actors are
//! tied together with message calls.
//!
//! In this loop the exit checks it's database for registered users and deploys the endpoint for
//! their exit tunnel the execution model for all of this is pretty whacky thanks to Actix quirks
//! we have the usual actors, these actors process Async events, but we have database queries by
//! Diesel that are sync so we create a special futures executor thread that runs only a single blocking
//! future. Since it's another thread
//!
//! Two threads are generated by this, one actual worker thread and a watchdog restarting thread that only
//! wakes up to restart the inner thread if anything goes wrong.

use crate::{get_database_connection, network_endpoints::*, RitaExitError};

use crate::database::struct_tools::clients_to_ids;
use crate::database::{
    cleanup_exit_clients, enforce_exit_clients, setup_clients, validate_clients_region,
    ExitClientSetupStates,
};
use crate::traffic_watcher::{watch_exit_traffic, Watch};
use actix_async::System as AsyncSystem;
use actix_web_async::{web, App, HttpServer};
use althea_kernel_interface::ExitClient;
use althea_types::{Identity, WgKey};
use babel_monitor::{open_babel_stream, parse_routes};

use diesel::{query_dsl::RunQueryDsl, PgConnection};
use exit_db::models;
use exit_db::schema::clients::internet_ipv6;
use rita_common::debt_keeper::DebtAction;
use settings::{get_rita_exit, set_rita_exit, write_config};

use std::collections::HashSet;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use rita_common::KI;

// the speed in seconds for the exit loop
pub const EXIT_LOOP_SPEED: u64 = 5;
pub const EXIT_LOOP_SPEED_DURATION: Duration = Duration::from_secs(EXIT_LOOP_SPEED);
pub const EXIT_LOOP_TIMEOUT: Duration = Duration::from_secs(4);

/// Cache of rita exit state to track across ticks
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct RitaExitCache {
    // a cache of what tunnels we had setup last round, used to prevent extra setup ops
    wg_clients: HashSet<ExitClient>,
    // a list of client debts from the last round, to prevent extra enforcement ops
    debt_actions: HashSet<(Identity, DebtAction)>,
    // if we have successfully setup the wg exit tunnel in the past, if false we have never
    // setup exit clients and should crash if we fail to do so, otherwise we are preventing
    // proper failover
    successful_setup: bool,
    // cache of b19 routers we have successful rules and routes for
    wg_exit_clients: HashSet<WgKey>,
    // cache of b20 routers we have successful rules and routes for
    wg_exit_v2_clients: HashSet<WgKey>,
}

/// Starts the rita exit billing thread, this thread deals with blocking db
/// calls and performs various tasks required for billing. The tasks interacting
/// with actix are the most troublesome because the actix system may restart
/// and crash this thread. To prevent that and other crashes we have a watchdog
/// thread which simply restarts the billing.
/// TODO remove futures on the non http endpoint / actix parts of this
/// TODO remove futures on the actix parts of this by moving to thread local state
pub fn start_rita_exit_loop() {
    setup_exit_wg_tunnel();
    let mut last_restart = Instant::now();
    // outer thread is a watchdog, inner thread is the runner
    thread::spawn(move || {
        // this will always be an error, so it's really just a loop statement
        // with some fancy destructuring
        while let Err(e) = {
            thread::spawn(move || {
                // Internal exit cache that store state across multiple ticks
                let mut rita_exit_cache = RitaExitCache::default();

                loop {
                    rita_exit_cache = rita_exit_loop(rita_exit_cache);
                }
            })
            .join()
        } {
            error!("Exit loop thread panicked! Respawning {:?}", e);
            if Instant::now() - last_restart < Duration::from_secs(60) {
                error!("Restarting too quickly, leaving it to systemd!");
                let sys = AsyncSystem::current();
                sys.stop_with_code(121);
            }
            last_restart = Instant::now();
        }
    });
}

fn rita_exit_loop(rita_exit_cache: RitaExitCache) -> RitaExitCache {
    let mut rita_exit_cache = rita_exit_cache;
    let start = Instant::now();
    // opening a database connection takes at least several milliseconds, as the database server
    // may be across the country, so to save on back and forth we open on and reuse it as much
    // as possible
    match get_database_connection() {
        Ok(conn) => {
            use exit_db::schema::clients::dsl::clients;
            let babel_port = settings::get_rita_exit().network.babel_port;
            info!(
                "Exit tick! got DB connection after {}ms",
                start.elapsed().as_millis(),
            );

            // Resets all ipv6 data in database
            if let Err(e) = recompute_ipv6_if_needed(&conn) {
                error!("IPV6 Error: Unable to reset databases: {:?}", e);
            };

            let get_clients = Instant::now();
            if let Ok(clients_list) = clients.load::<models::Client>(&conn) {
                info!(
                    "Finished Rita get clients, got {:?} clients in {}ms",
                    clients_list.len(),
                    get_clients.elapsed().as_millis()
                );
                let ids = clients_to_ids(clients_list.clone());

                let start_bill = Instant::now();
                // watch and bill for traffic
                bill(babel_port, start, ids);
                info!(
                    "Finished Rita billing in {}ms",
                    start_bill.elapsed().as_millis()
                );

                info!("about to setup clients");
                let start_setup = Instant::now();
                // Create and update client tunnels
                match setup_clients(
                    &clients_list,
                    ExitClientSetupStates {
                        old_clients: rita_exit_cache.wg_clients.clone(),
                        wg_exit_clients: rita_exit_cache.wg_exit_clients.clone(),
                        wg_exit_v2_clients: rita_exit_cache.wg_exit_v2_clients.clone(),
                    },
                ) {
                    Ok(client_states) => {
                        rita_exit_cache.successful_setup = true;
                        rita_exit_cache.wg_clients = client_states.old_clients;
                        rita_exit_cache.wg_exit_clients = client_states.wg_exit_clients;
                        rita_exit_cache.wg_exit_v2_clients = client_states.wg_exit_v2_clients;
                    }
                    Err(e) => error!("Setup clients failed with {:?}", e),
                }
                info!(
                    "Finished Rita setting up clients in {}ms",
                    start_setup.elapsed().as_millis()
                );

                let start_cleanup = Instant::now();
                info!("about to cleanup clients");
                // find users that have not been active within the configured time period
                // and remove them from the db
                if let Err(e) = cleanup_exit_clients(&clients_list, &conn) {
                    error!("Exit client cleanup failed with {:?}", e);
                }
                info!(
                    "Finished Rita cleaning clients in {}ms",
                    start_cleanup.elapsed().as_millis()
                );

                // Make sure no one we are setting up is geoip unauthorized
                let start_region = Instant::now();
                info!("about to check regions");
                check_regions(start, clients_list.clone(), &conn);
                info!(
                    "Finished Rita checking region in {}ms",
                    start_region.elapsed().as_millis()
                );

                info!("About to enforce exit clients");
                // handle enforcement on client tunnels by querying debt keeper
                // this consumes client list
                let start_enforce = Instant::now();
                match enforce_exit_clients(clients_list, &rita_exit_cache.debt_actions) {
                    Ok(new_debt_actions) => rita_exit_cache.debt_actions = new_debt_actions,
                    Err(e) => warn!("Failed to enforce exit clients with {:?}", e,),
                }
                info!(
                    "Finished Rita enforcement in {}ms ",
                    start_enforce.elapsed().as_millis()
                );

                info!(
                    "Finished Rita exit loop in {}ms, all vars should be dropped",
                    start.elapsed().as_millis(),
                );
            }
        }
        Err(e) => {
            error!("Failed to get database connection with {}", e);
            if !rita_exit_cache.successful_setup {
                let db_uri = settings::get_rita_exit().db_uri;
                let message = format!(
                    "Failed to get database connection to {db_uri} on first setup loop, the exit can not operate without the ability to get the clients list from the database exiting"
                );
                error!("{}", message);
                let sys = AsyncSystem::current();
                sys.stop();
                panic!("{}", message);
            }
        }
    }
    thread::sleep(EXIT_LOOP_SPEED_DURATION);
    rita_exit_cache
}

fn bill(babel_port: u16, start: Instant, ids: Vec<Identity>) {
    trace!("about to try opening babel stream");

    match open_babel_stream(babel_port, EXIT_LOOP_TIMEOUT) {
        Ok(mut stream) => match parse_routes(&mut stream) {
            Ok(routes) => {
                trace!("Sending traffic watcher message?");
                if let Err(e) = watch_exit_traffic(Watch { users: ids, routes }) {
                    error!(
                        "Watch exit traffic failed with {}, in {} millis",
                        e,
                        start.elapsed().as_millis()
                    );
                } else {
                    info!(
                        "Watch exit traffic completed successfully in {} millis",
                        start.elapsed().as_millis()
                    );
                }
            }
            Err(e) => {
                error!(
                    "Watch exit traffic failed with: {} in {} millis",
                    e,
                    start.elapsed().as_millis()
                );
            }
        },
        Err(e) => {
            error!(
                "Watch exit traffic failed with: {} in {} millis",
                e,
                start.elapsed().as_millis()
            );
        }
    }
}

fn check_regions(start: Instant, clients_list: Vec<models::Client>, conn: &PgConnection) {
    let val = settings::get_rita_exit().allowed_countries.is_empty();
    if !val {
        let res = validate_clients_region(clients_list, conn);
        match res {
            Err(e) => warn!(
                "Failed to validate client region with {:?} {}ms since start",
                e,
                start.elapsed().as_millis()
            ),
            Ok(_) => info!(
                "validate client region completed successfully {}ms since loop start",
                start.elapsed().as_millis()
            ),
        }
    }
}

/// When the ipv6 database gets into an invalid state, we can have unexpected behaviors if rita exit doesnt
/// crash. This function checks if a config variable is set; if it is, clear out ipv6 database and let it recompute
fn recompute_ipv6_if_needed(conn: &PgConnection) -> Result<(), Box<RitaExitError>> {
    use diesel::ExpressionMethods;
    use exit_db::schema::assigned_ips::dsl::assigned_ips;
    use exit_db::schema::clients::dsl::clients;
    let mut rita_exit = get_rita_exit();
    let recompute = rita_exit.exit_network.recompute_ipv6;
    info!("About to call recompute with : {:?}", recompute);
    if recompute {
        info!("Reseting IPV6 databases");

        // Reseting client ipv6 column
        let empty_str = "";
        if let Err(e) = diesel::update(clients)
            .set(internet_ipv6.eq(empty_str))
            .execute(conn)
        {
            return Err(Box::new(e.into()));
        };

        // Reseting assigned_ips
        if let Err(e) = diesel::delete(assigned_ips).execute(conn) {
            return Err(Box::new(e.into()));
        };

        // Set recompute ipv6 to false
        rita_exit.exit_network.recompute_ipv6 = false;
        set_rita_exit(rita_exit);

        if let Err(e) = write_config() {
            error!("Unable to write to config with: {:?}", e);
        }
    }

    Ok(())
}

fn setup_exit_wg_tunnel() {
    // Setup legacy wg_exit
    if let Err(e) = KI.setup_wg_if_named("wg_exit") {
        warn!("exit setup returned {}", e)
    }
    // Setup new wg_exit
    if let Err(e) = KI.setup_wg_if_named("wg_exit_v2") {
        warn!("new exit setup returned {}", e)
    }

    let exit_settings = settings::get_rita_exit();

    let local_ip = exit_settings.exit_network.own_internal_ip.into();
    let netmask = exit_settings.exit_network.netmask;
    let mesh_ip = exit_settings
        .network
        .mesh_ip
        .expect("Expected a mesh ip for this exit");
    let ex_nic = exit_settings
        .network
        .external_nic
        .expect("Expected an external nic here");
    let enforcement_enabled = exit_settings.exit_network.enable_enforcement;
    let external_v6 = exit_settings
        .exit_network
        .subnet
        .map(|ipv6_subnet| (ipv6_subnet.ip(), ipv6_subnet.prefix()));

    // Setup legacy wg_exit
    KI.one_time_exit_setup(
        None,
        None,
        mesh_ip,
        ex_nic.clone(),
        "wg_exit",
        enforcement_enabled,
    )
    .expect("Failed to setup wg_exit!");

    // Setup wg_exit_v2. Local address added is same as that used by wg_exit
    KI.one_time_exit_setup(
        Some((local_ip, netmask)),
        external_v6,
        mesh_ip,
        ex_nic,
        "wg_exit_v2",
        enforcement_enabled,
    )
    .expect("Failed to setup wg_exit_v2!");

    KI.setup_nat(
        &settings::get_rita_exit().network.external_nic.unwrap(),
        "wg_exit",
    )
    .unwrap();
    KI.setup_nat(
        &settings::get_rita_exit().network.external_nic.unwrap(),
        "wg_exit_v2",
    )
    .unwrap();
}

pub fn start_rita_exit_endpoints(workers: usize) {
    thread::spawn(move || {
        let runner = AsyncSystem::new();
        runner.block_on(async move {
            // Exit stuff, huge threadpool to offset Pgsql blocking
            let _res = HttpServer::new(|| {
                App::new()
                    .route("/secure_setup", web::post().to(secure_setup_request))
                    .route("/secure_status", web::post().to(secure_status_request))
                    .route("/exit_info", web::get().to(get_exit_info_http))
                    .route("/client_debt", web::post().to(get_client_debt))
                    .route("/time", web::get().to(get_exit_timestamp_http))
                    .route("/exit_list", web::post().to(get_exit_list))
            })
            .workers(workers)
            .bind(format!(
                "[::0]:{}",
                settings::get_rita_exit().exit_network.exit_hello_port
            ))
            .unwrap()
            .shutdown_timeout(0)
            .run()
            .await;
        });
    });
}
