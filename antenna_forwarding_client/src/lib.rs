#![warn(clippy::all)]
#![allow(clippy::pedantic)]
#![forbid(unsafe_code)]

#[macro_use]
extern crate log;
#[macro_use]
extern crate failure;
#[macro_use]
extern crate lazy_static;

use althea_kernel_interface::KernelInterface;
use althea_kernel_interface::LinuxCommandRunner;
use althea_types::Identity;
use althea_types::WgKey;
use antenna_forwarding_protocol::process_streams;
use antenna_forwarding_protocol::write_all_spinlock;
use antenna_forwarding_protocol::ForwardingProtocolMessage;
use antenna_forwarding_protocol::NET_TIMEOUT;
use antenna_forwarding_protocol::SPINLOCK_TIME;
use failure::Error;
use oping::Ping;
use rand::Rng;
use std::collections::HashMap;
use std::collections::HashSet;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::Shutdown;
use std::net::SocketAddr;
use std::net::TcpStream;
use std::thread;
use std::time::Duration;
use std::time::Instant;

lazy_static! {
    pub static ref KI: Box<dyn KernelInterface> = Box::new(LinuxCommandRunner {});
}

const SLEEP_TIME: Duration = NET_TIMEOUT;
/// The timeout time for pinging a local antenna, 25ms is very
/// very generous here as they should all respond really within 5ms
const PING_TIMEOUT: Duration = Duration::from_millis(100);
/// the amount of time with no activity before we close a forwarding session
const FORWARD_TIMEOUT: Duration = Duration::from_secs(600);

/// Starts a thread that will check in with the provided server repeatedly and forward antennas
/// when the right signal is recieved. The type bound is so that you can use custom hashers and
/// may not really be worth keeping around.
pub fn start_antenna_forwarding_proxy<S: 'static + std::marker::Send + ::std::hash::BuildHasher>(
    checkin_address: String,
    our_id: Identity,
    _server_public_key: WgKey,
    _our_public_key: WgKey,
    _our_private_key: WgKey,
    interfaces_to_search: HashSet<String, S>,
) {
    info!("Starting antenna forwarding proxy!");
    let socket: SocketAddr = match checkin_address.parse() {
        Ok(socket) => socket,
        Err(_) => {
            error!("Could not parse {}!", checkin_address);
            return;
        }
    };

    thread::spawn(move || loop {
        // parse checkin address every loop iteration as a way
        // of resolving the domain name on each run
        trace!("About to checkin with {}", checkin_address);
        thread::sleep(SLEEP_TIME);
        if let Ok(mut server_stream) = TcpStream::connect_timeout(&socket, NET_TIMEOUT) {
            trace!("connected to {}", checkin_address);
            // send our identifier
            let _res = write_all_spinlock(
                &mut server_stream,
                &ForwardingProtocolMessage::new_identification_message(our_id).get_message(),
            );
            // wait for a NET_TIMEOUT and see if the server responds, then read it's entire response
            thread::sleep(NET_TIMEOUT);
            match ForwardingProtocolMessage::read_messages(&mut server_stream) {
                Ok(messages) => {
                    // read messages will return a vec of at least one,
                    if let Some(ForwardingProtocolMessage::ForwardMessage {
                        ip,
                        server_port: _server_port,
                        antenna_port,
                    }) = messages.iter().next()
                    {
                        // if there are other messages in this batch safely form a slice
                        // to pass on
                        let slice = if messages.len() > 1 {
                            &messages[1..]
                        } else {
                            // an empty slice
                            &([] as [ForwardingProtocolMessage; 0])
                        };
                        // setup networking and process the rest of the messages in this batch
                        match setup_networking(*ip, *antenna_port, &interfaces_to_search) {
                            Ok(antenna_sockaddr) => {
                                forward_connections(antenna_sockaddr, server_stream, slice);
                            }
                            Err(e) => send_error_message(&mut server_stream, format!("{:?}", e)),
                        }
                    } else {
                        error!("Wrong start message!");
                    }
                }
                Err(e) => {
                    error!("Failed to read message from server with {:?}", e);
                    continue;
                }
            }
        }
        trace!("Waiting for next checkin cycle");
    });
}

/// Processes an array of messages and takes the appropriate actions
/// returns if the forwarder should shutdown becuase a shutdown message
/// was found in the message batch.
fn process_messages(
    input: &[ForwardingProtocolMessage],
    streams: &mut HashMap<u64, TcpStream>,
    server_stream: &mut TcpStream,
    last_message: &mut Instant,
    antenna_sockaddr: SocketAddr,
) -> bool {
    for item in input {
        match item {
            // why would the server ID themselves to us?
            ForwardingProtocolMessage::IdentificationMessage { .. } => unimplemented!(),
            // two forward messages?
            ForwardingProtocolMessage::ForwardMessage { .. } => unimplemented!(),
            // the server doesn't send us error messages, what would we do with it?
            ForwardingProtocolMessage::ErrorMessage { .. } => unimplemented!(),
            ForwardingProtocolMessage::ConnectionCloseMessage { stream_id } => {
                trace!("Got close message for stream {}", stream_id);
                *last_message = Instant::now();
                let stream_id = stream_id;
                let stream = streams
                    .get(stream_id)
                    .expect("How can we close a stream we don't have?");
                stream
                    .shutdown(Shutdown::Both)
                    .expect("Failed to shutdown connection!");
                streams.remove(stream_id);
            }
            ForwardingProtocolMessage::ConnectionDataMessage { stream_id, payload } => {
                trace!(
                    "Got connection message for stream {} payload {} bytes",
                    stream_id,
                    payload.len()
                );
                *last_message = Instant::now();
                let stream_id = stream_id;
                if let Some(mut antenna_stream) = streams.get_mut(stream_id) {
                    write_all_spinlock(&mut antenna_stream, &payload)
                        .expect("Failed to talk to antenna!");
                } else {
                    trace!("Opening stream for {}", stream_id);
                    // we don't have a stream, we need to dial out to the server now
                    let mut new_stream =
                        TcpStream::connect(antenna_sockaddr).expect("Could not contact antenna!");
                    write_all_spinlock(&mut new_stream, &payload)
                        .expect("Failed to talk to antenna!");
                    streams.insert(*stream_id, new_stream);
                }
            }
            ForwardingProtocolMessage::ForwardingCloseMessage => {
                trace!("Got halt message");
                // we have a close lets get out of here.
                for stream in streams.values_mut() {
                    stream
                        .shutdown(Shutdown::Both)
                        .expect("Failed to shutdown connection!");
                }
                server_stream
                    .shutdown(Shutdown::Both)
                    .expect("Could not shutdown connection!");
                return true;
            }
            // we don't use this yet
            ForwardingProtocolMessage::KeepAliveMessage => unimplemented!(),
        }
    }
    false
}

/// Actually forwards the connection by managing the reading and writing from
/// various tcp sockets
fn forward_connections(
    antenna_sockaddr: SocketAddr,
    server_stream: TcpStream,
    first_round_input: &[ForwardingProtocolMessage],
) {
    trace!("Forwarding connections!");
    let mut server_stream = server_stream;
    let mut streams: HashMap<u64, TcpStream> = HashMap::new();
    let mut last_message = Instant::now();
    process_messages(
        first_round_input,
        &mut streams,
        &mut server_stream,
        &mut last_message,
        antenna_sockaddr,
    );

    while let Ok(vec) = ForwardingProtocolMessage::read_messages(&mut server_stream) {
        process_streams(&mut streams, &mut server_stream);
        let should_shutdown = process_messages(
            &vec,
            &mut streams,
            &mut server_stream,
            &mut last_message,
            antenna_sockaddr,
        );
        if should_shutdown {
            break;
        }

        if Instant::now() - last_message > FORWARD_TIMEOUT {
            error!("Fowarding session timed out!");
            break;
        }
        thread::sleep(SPINLOCK_TIME);
    }
}

/// handles the setup of networking to the selected antenna, including finding it and the like
/// returns a socketaddr for the antenna
fn setup_networking<S: ::std::hash::BuildHasher>(
    antenna_ip: IpAddr,
    antenna_port: u16,
    interfaces: &HashSet<String, S>,
) -> Result<SocketAddr, Error> {
    match find_antenna(antenna_ip, interfaces) {
        Ok(_iface) => {}
        Err(e) => {
            error!("Could not find anntenna {:?}", e);
            return Err(e);
        }
    };
    Ok(SocketAddr::new(antenna_ip, antenna_port))
}

/// Finds the antenna on the appropriate physical interface by iterating
/// over the list of provided interfaces, attempting a ping
/// and repeating until the appropriate interface is located
/// TODO handle overlapping edge cases for gateway ip, lan ip, br-pbs etc
fn find_antenna<S: ::std::hash::BuildHasher>(
    ip: IpAddr,
    interfaces: &HashSet<String, S>,
) -> Result<String, Error> {
    let our_ip = get_local_ip(ip);
    for iface in interfaces {
        trace!("Trying interface {}, with test ip {}", iface, our_ip);
        // this acts as a wildcard deletion across all interfaces, which is frankly really
        // dangerous if our default route overlaps, of if you enter an exit route ip
        let _ = KI.run_command("ip", &["route", "del", &format!("{}/32", ip)]);
        for iface in interfaces {
            let _ = KI.run_command(
                "ip",
                &["addr", "del", &format!("{}/32", our_ip), "dev", iface],
            );
        }
        let res = KI.run_command(
            "ip",
            &["addr", "add", &format!("{}/32", our_ip), "dev", iface],
        );
        trace!("Added our own test ip with {:?}", res);
        // you need to use src here to disambiguate the sending address
        // otherwise the first avaialble ipv4 address on the interface will
        // be used
        match KI.run_command(
            "ip",
            &[
                "route",
                "add",
                &format!("{}/32", ip),
                "dev",
                iface,
                "src",
                &our_ip.to_string(),
            ],
        ) {
            Ok(r) => {
                // exit status 512 is the code for 'file exists' meaning we are not
                // checking the interface we thought we where. At this point there's
                // no option but to exit
                if let Some(code) = r.status.code() {
                    if code == 512 {
                        error!("Failed to add route");
                        bail!("IP setup failed");
                    }
                }
                trace!("added route with {:?}", r);
            }
            Err(e) => {
                trace!("Failed to add route with {:?}", e);
                continue;
            }
        }
        let mut pinger = Ping::new();
        pinger.set_timeout((PING_TIMEOUT.as_millis() as f64 / 1000f64) as f64)?;
        pinger.add_host(&ip.to_string())?;
        let mut response = match pinger.send() {
            Ok(res) => res,
            Err(e) => {
                trace!("Failed to ping with {:?}", e);
                continue;
            }
        };
        if let Some(res) = response.next() {
            trace!("got ping response {:?}", res);
            if res.dropped == 0 {
                return Ok((*iface).to_string());
            }
        }
    }
    Err(format_err!("Failed to find Antenna!"))
}

/// Generates a random non overlapping ip within a /24 subnet of the provided
/// target antenna ip.
fn get_local_ip(target_ip: IpAddr) -> IpAddr {
    match target_ip {
        IpAddr::V4(address) => {
            let mut rng = rand::thread_rng();
            let mut bytes = address.octets();
            let mut new_ip: u8 = rng.gen();
            // keep trying until we get a different number
            // only editing the last byte is implicitly working
            // within a /24
            while new_ip == bytes[3] {
                new_ip = rng.gen()
            }
            bytes[3] = new_ip;
            Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).into()
        }
        IpAddr::V6(_address) => unimplemented!(),
    }
}

fn send_error_message(server_stream: &mut TcpStream, message: String) {
    let msg = ForwardingProtocolMessage::new_error_message(message);
    let _res = write_all_spinlock(server_stream, &msg.get_message());
    let _res = server_stream.shutdown(Shutdown::Both);
}