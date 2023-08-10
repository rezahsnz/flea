#![doc = include_str!("../README.md")]

use futures::{future::Either, prelude::*, select};
use libp2p::{
    core::{muxing::StreamMuxerBox, transport::OrTransport, upgrade},
    gossipsub, identity, mdns, noise, request_response,
    swarm::NetworkBehaviour,
    swarm::{StreamProtocol, SwarmBuilder, SwarmEvent},
    tcp, yamux, PeerId, Transport,
};
use libp2p_quic as quic;
use num_enum::TryFromPrimitive;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::fs;
use std::hash::{Hash, Hasher};
use std::time::Duration;
use toml;

// config
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Config {
    role: Option<String>,
    docker_image: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
enum Role {
    Client,
    Server,
    Verifier,
    All,
}

#[derive(Debug, TryFromPrimitive, Eq, PartialEq)]
#[repr(u8)]
enum RequestType {
    Compute = 1,      // I need compute resources
    Verification = 2, // I need to verify a computation
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ComputeRequest {
    docker_image: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ComputeResponse {
    docker_image: String,
}

// combine Gossipsub and mDNS
#[derive(NetworkBehaviour)]
struct MyBehaviour {
    req_resp: request_response::cbor::Behaviour<ComputeRequest, ComputeResponse>,
    gossipsub: gossipsub::Behaviour,
    mdns: mdns::async_io::Behaviour,
}

#[async_std::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // load the config file
    //println!("{:?}", std::env::current_dir());

    let config: Config = {
        let config_str =
            fs::read_to_string("./target/release/config.toml").expect("Config file is not found.");
        toml::from_str(config_str.as_str())?
    };
    println!("{:#?}", config);
    let mut my_role: Role = Role::All;
    if config.role.is_some() {
        my_role = match config.role.unwrap().to_lowercase().as_str() {
            "client" => Role::Client,
            "server" => Role::Server,
            "verifier" => Role::Verifier,
            _ => Role::All,
        };
    }
    // get a random peer_id
    let id_keys = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(id_keys.public());
    println!("PeerId: {local_peer_id}");
    // setup an encrypted dns-enabled transport over yamux
    let tcp_transport = tcp::async_io::Transport::new(tcp::Config::default().nodelay(true))
        .upgrade(upgrade::Version::V1Lazy)
        .authenticate(noise::Config::new(&id_keys).expect("signing libp2p static keypair"))
        .multiplex(yamux::Config::default())
        .timeout(std::time::Duration::from_secs(30))
        .boxed();
    let quic_transport = quic::async_std::Transport::new(quic::Config::new(&id_keys));
    let transport = OrTransport::new(quic_transport, tcp_transport)
        .map(|either_output, _| match either_output {
            Either::Left((peer_id, muxer)) => (peer_id, StreamMuxerBox::new(muxer)),
            Either::Right((peer_id, muxer)) => (peer_id, StreamMuxerBox::new(muxer)),
        })
        .boxed();

    // to content-address message, take the hash of message and use it as an id
    let message_id_fn = |message: &gossipsub::Message| {
        let mut s = DefaultHasher::new();
        message.data.hash(&mut s);
        gossipsub::MessageId::from(s.finish().to_string())
    };

    // set a custom Gossipsub configuration
    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_secs(10)) // aid debugging by not cluttering log space
        .validation_mode(gossipsub::ValidationMode::Strict) // enforce message signing
        .message_id_fn(message_id_fn) // content-address messages
        .build()
        .expect("Invalid gossipsub config.");

    // build a Gossipsub network behaviour
    let mut gossipsub = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(id_keys),
        gossipsub_config,
    )
    .expect("Invalid behaviour configuration.");

    // subscribe to our topic
    const TOPIC_OF_INTEREST: &str = "<-- Compute Bazaar -->";
    println!("topic of interest: `{TOPIC_OF_INTEREST}`");
    // @ use topic_hash config for auto hash(topic)
    let topic = gossipsub::IdentTopic::new(TOPIC_OF_INTEREST);
    let _ = gossipsub.subscribe(&topic);

    // create a swarm to manage events and peers
    let mut swarm = {
        let mdns = mdns::async_io::Behaviour::new(mdns::Config::default(), local_peer_id)?;
        let req_resp = request_response::cbor::Behaviour::<ComputeRequest, ComputeResponse>::new(
            [(
                StreamProtocol::new("/p2pcompute"),
                request_response::ProtocolSupport::Full,
            )],
            request_response::Config::default(),
        );
        let behaviour = MyBehaviour {
            req_resp: req_resp,
            gossipsub: gossipsub,
            mdns: mdns,
        };
        SwarmBuilder::with_async_std_executor(transport, behaviour, local_peer_id).build()
    };

    // read full lines from stdin
    // let mut input = io::BufReader::new(io::stdin()).lines().fuse();

    // listen on all interfaces and whatever port the os assigns
    swarm.listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse()?)?;
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;
    // kick it off
    loop {
        select! {
            // line = input.select_next_some() => {
            //   if let Err(e) = swarm
            //     .behaviour_mut().gossipsub
            //     .publish(topic.clone(), line.expect("Stdin not to close").as_bytes()) {
            //       println!("Publish error: {e:?}")
            //     }
            // },
            event = swarm.select_next_some() => match event {
            SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                for (peer_id, _multiaddr) in list {
                    println!("mDNS discovered a new peer: {peer_id}");
                    swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                }
            },

            SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
                for (peer_id, _multiaddr) in list {
                    println!("mDNS discovered peer has expired: {peer_id}");
                    swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                }
            },

            SwarmEvent::Behaviour(MyBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source: peer_id,
                // message_id: message_id,
                message,
                ..
            })) => {
                // let msg_str = String::from_utf8_lossy(&message.data);
                // println!("Got message: '{}' with id: {id} from peer: {peer_id}",
                //          msg_str);
                println!("{:#?}", message);
                // first byte is message identifier
                let req_type = RequestType::try_from(message.data[0])?;
                match req_type {
                    RequestType::Compute => {
                        if (my_role == Role::Client) || (my_role == Role::Verifier) {
                            // skip
                            continue;
                        }
                        println!("new Compute request...");
                        // engage with the client through a direct p2p channel
                        // and express interest in getting the compute done
                        let req_id = swarm
                            .behaviour_mut().req_resp
                            .send_request(
                                &peer_id,
                                ComputeRequest {docker_image: "aloha".to_string()},
                            );
                        println!("request was sent, id: {req_id}");
                    },

                    RequestType::Verification => {
                        if (my_role == Role::Client) || (my_role == Role::Server) {
                            // only verifiers do this
                            continue;
                        }
                        println!("new verification request...");
                        // engage with the client through a direct p2p channel
                    },
                };
            },
            // incoming compute request from a server
            SwarmEvent::Behaviour(MyBehaviourEvent::ReqResp(request_response::Event::Message{
                peer: sender_peer_id,
                message: request_response::Message::Request {
                    request,
                    channel,
                    ..
                }
            })) => {
                println!("request came from {sender_peer_id}, req: {:#?}, chan: {:#?}",
                    request, channel);
            },

            SwarmEvent::NewListenAddr { address, .. } => {
              println!("Local node is listening on {address}");
            }

            _ => {}

          },
        }
    }
}
