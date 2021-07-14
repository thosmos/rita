//! PeerListener is used to detect nearby mesh peers, it listens on a ff02::/8 ipv6 address, which is
//! a link local multicast address, on each listen port.
//!
//! On initialization a set of ListenInterface objects are created, these are important because they
//! actually hold the sockets required to listen and broadcast on the listen interfaces, every
//! rita_loop iteration we send out our own IP as a UDP broadcast packet and then get our peers
//! off the queue. These are turned into Peer structs which are passed to TunnelManager to do
//! whatever remaining work there may be.

mod message;

use self::message::PeerMessage;
use crate::KI;
use crate::SETTING;
use failure::Error;
use settings::RitaCommonSettings;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr, SocketAddrV6, UdpSocket};
use std::sync::Arc;
use std::sync::RwLock;

lazy_static! {
    static ref PEER_LISTENER: Arc<RwLock<PeerListener>> =
        Arc::new(RwLock::new(PeerListener::default()));
}

#[derive(Debug)]
pub struct PeerListener {
    interfaces: HashMap<String, ListenInterface>,
    peers: HashMap<IpAddr, Peer>,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Peer {
    pub ifidx: u32,
    pub contact_socket: SocketAddr,
}

impl Peer {
    pub fn new(ip: Ipv6Addr, idx: u32) -> Peer {
        let port = SETTING.get_network().rita_hello_port;
        let socket = SocketAddrV6::new(ip, port, 0, idx);
        Peer {
            ifidx: idx,
            contact_socket: socket.into(),
        }
    }
}

impl Default for PeerListener {
    fn default() -> PeerListener {
        PeerListener::new().unwrap()
    }
}

impl PeerListener {
    pub fn new() -> Result<PeerListener, Error> {
        Ok(PeerListener {
            interfaces: HashMap::new(),
            peers: HashMap::new(),
        })
    }
}

fn listen_to_available_ifaces(peer_listener: &mut PeerListener) {
    let interfaces = SETTING.get_network().peer_interfaces.clone();
    let iface_list = interfaces;
    for iface in iface_list.iter() {
        if !peer_listener.interfaces.contains_key(iface) {
            match ListenInterface::new(iface) {
                Ok(new_listen_interface) => {
                    peer_listener
                        .interfaces
                        .insert(new_listen_interface.ifname.clone(), new_listen_interface);
                }
                Err(_e) => {}
            }
        }
    }
}

pub fn tick() {
    trace!("Starting PeerListener tick!");

    let mut writer = PEER_LISTENER.write().unwrap();
    send_im_here(&mut writer.interfaces);

    (*writer).peers = receive_im_here(&mut writer.interfaces);

    listen_to_available_ifaces(&mut writer);
}

#[allow(dead_code)]
pub fn unlisten_interface(interface: String) {
    trace!("Peerlistener unlisten on {:?}", interface);
    let ifname_to_delete = interface;
    let mut writer = PEER_LISTENER.write().unwrap();
    if writer.interfaces.contains_key(&ifname_to_delete) {
        writer.interfaces.remove(&ifname_to_delete);
        SETTING
            .get_network_mut()
            .peer_interfaces
            .remove(&ifname_to_delete);
    } else {
        error!("Tried to unlisten interface that's not present!")
    }
}

pub fn get_peers() -> HashMap<IpAddr, Peer> {
    PEER_LISTENER.read().unwrap().peers.clone()
}

#[derive(Debug)]
pub struct ListenInterface {
    ifname: String,
    ifidx: u32,
    multicast_socketaddr: SocketAddrV6,
    multicast_socket: UdpSocket,
    linklocal_socket: UdpSocket,
    linklocal_ip: Ipv6Addr,
}

impl ListenInterface {
    pub fn new(ifname: &str) -> Result<ListenInterface, Error> {
        let port = SETTING.get_network().rita_hello_port;
        let disc_ip = SETTING.get_network().discovery_ip;
        debug!("Binding to {:?} for ListenInterface", ifname);
        // Lookup interface link local ip
        let link_ip = KI.get_link_local_device_ip(&ifname)?;

        // Lookup interface index
        let iface_index = KI.get_iface_index(&ifname).unwrap_or(0);
        // Bond to multicast discovery address on each listen port
        let multicast_socketaddr = SocketAddrV6::new(disc_ip, port, 0, iface_index);

        // try_link_ip should guard from non-existant interfaces and the network stack not being ready
        // so in theory we should never hit this expect or the panic below either.
        let multicast_socket = UdpSocket::bind(multicast_socketaddr)
            .expect("Failed to bind to peer discovery address!");
        let res = multicast_socket.join_multicast_v6(&disc_ip, iface_index);
        trace!("ListenInterface init set multicast v6 with {:?}", res);
        let res = multicast_socket.set_nonblocking(true);
        trace!(
            "ListenInterface multicast init set nonblocking with {:?}",
            res
        );

        let linklocal_socketaddr = SocketAddrV6::new(link_ip, port, 0, iface_index);
        let linklocal_socket = UdpSocket::bind(linklocal_socketaddr)?;
        let res = linklocal_socket.set_nonblocking(true);
        trace!("ListenInterface init set nonblocking with {:?}", res);

        let res = linklocal_socket.join_multicast_v6(&disc_ip, iface_index);
        trace!("ListenInterface Set link local multicast v6 with {:?}", res);

        Ok(ListenInterface {
            ifname: ifname.to_string(),
            ifidx: iface_index,
            multicast_socket,
            linklocal_socket,
            multicast_socketaddr,
            linklocal_ip: link_ip,
        })
    }
}

fn send_im_here(interfaces: &mut HashMap<String, ListenInterface>) {
    trace!("About to send ImHere");
    for obj in interfaces.iter_mut() {
        let listen_interface = obj.1;
        trace!(
            "Sending ImHere to {:?}, with ip {:?}",
            listen_interface.ifname,
            listen_interface.linklocal_ip
        );
        let message = PeerMessage::ImHere(listen_interface.linklocal_ip);
        let result = listen_interface
            .linklocal_socket
            .send_to(&message.encode(), listen_interface.multicast_socketaddr);
        trace!("Sending ImHere to broadcast gets {:?}", result);
        if result.is_err() {
            info!("Sending ImHere failed with {:?}", result);
        }
    }
}

fn receive_im_here(interfaces: &mut HashMap<String, ListenInterface>) -> HashMap<IpAddr, Peer> {
    trace!("About to dequeue ImHere");
    let mut output = HashMap::<IpAddr, Peer>::new();
    for obj in interfaces.iter_mut() {
        let listen_interface = obj.1;
        // Since the only datagrams we are interested in are very small (22 bytes plus overhead)
        // this buffer is kept intentionally small to discard larger packets earlier rather than later
        loop {
            let mut datagram: [u8; 100] = [0; 100];
            let (bytes_read, sock_addr) =
                match listen_interface.multicast_socket.recv_from(&mut datagram) {
                    Ok(b) => b,
                    Err(e) => {
                        trace!("Could not recv ImHere: {:?}", e);
                        // TODO Consider we might want to remove interfaces that produce specific types
                        // of errors from the active list
                        break;
                    }
                };
            trace!(
                "Received {} bytes on multicast socket from {:?}",
                bytes_read,
                sock_addr
            );

            let ipaddr = match PeerMessage::decode(&datagram.to_vec()) {
                Ok(PeerMessage::ImHere(ipaddr)) => ipaddr,
                Err(e) => {
                    warn!("ImHere decode failed: {:?}", e);
                    continue;
                }
            };

            if ipaddr == listen_interface.linklocal_ip {
                trace!("Got ImHere from myself");
                continue;
            }

            if output.contains_key(&ipaddr.into()) {
                trace!(
                    "Discarding ImHere We already have a peer with {:?} for this cycle",
                    ipaddr
                );
                continue;
            }
            info!("ImHere with {:?}", ipaddr);
            let peer = Peer::new(ipaddr, listen_interface.ifidx);
            output.insert(peer.contact_socket.ip(), peer);
        }
    }
    output
}
