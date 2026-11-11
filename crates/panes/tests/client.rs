//! `HolderClient` against a scripted holder on a real unix socket.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread::{self, JoinHandle};

use tiller_panes::frame::{Frame, MAX_FRAME_LEN, read_frame, write_frame};
use tiller_panes::handshake::{ProtoRange, Refused, SUPPORTED, Welcome, negotiate};
use tiller_panes::message::{ErrorCode, HolderMessage, Request, SyncMode};
use tiller_panes::{
    Exit, HolderClient, Listing, Meta, Pane, PaneId, PaneState, Panes, PanesError, Spawn,
    Subscribe, Update, socket_path,
};

/// The holder's end of one connection.
struct Peer {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Peer {
    fn new(stream: UnixStream) -> Self {
        Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
        }
    }

    fn frame(&mut self) -> Option<Frame> {
        read_frame(&mut self.reader).unwrap()
    }

    fn request(&mut self) -> Request {
        match self.frame() {
            Some(Frame::Message(text)) => Request::decode(&text).unwrap(),
            other => panic!("expected a request, got {other:?}"),
        }
    }

    fn send(&mut self, message: &HolderMessage) {
        let text = serde_json::to_string(message).unwrap();
        write_frame(&self.writer, &Frame::Message(text)).unwrap();
    }

    fn send_frame(&mut self, frame: &Frame) {
        write_frame(&self.writer, frame).unwrap();
    }

    /// Reads the hello and answers as a holder that speaks `ours`.
    fn handshake(&mut self, ours: ProtoRange) -> Option<u32> {
        let Request::Hello(hello) = self.request() else {
            panic!("the first message was not a hello");
        };
        assert_eq!(hello.client, "daemon");
        assert_eq!(hello.proto, SUPPORTED);
        match negotiate(hello.proto, ours) {
            Some(proto) => {
                self.send(&HolderMessage::Welcome(welcome(proto)));
                Some(proto)
            }
            None => {
                self.send(&HolderMessage::Refused(Refused {
                    proto: ours,
                    message: "no common protocol version".into(),
                }));
                None
            }
        }
    }
}

const INSTANCE: &str = "01JDINSTANCE00000000000000";

fn welcome(proto: u32) -> Welcome {
    welcome_from(proto, INSTANCE)
}

fn welcome_from(proto: u32, instance: &str) -> Welcome {
    Welcome {
        proto,
        holder: "0.1.0-test".into(),
        instance: instance.into(),
        boot_id: "boot".into(),
        started_at: "2026-11-11T12:00:00Z".parse().unwrap(),
        features: Vec::new(),
    }
}

/// A holder socket in a temporary home, serving each connection in turn
/// with the next script.
struct Holder {
    _home: tempfile::TempDir,
    path: PathBuf,
    server: JoinHandle<()>,
}

type Script = Box<dyn FnOnce(Peer) + Send>;

impl Holder {
    fn serve(scripts: Vec<Script>) -> Self {
        let home = tempfile::tempdir().unwrap();
        let path = socket_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let mut connections = Vec::new();
            for script in scripts {
                let (stream, _) = listener.accept().unwrap();
                connections.push(thread::spawn(move || script(Peer::new(stream))));
            }
            for connection in connections {
                connection.join().unwrap();
            }
        });
        Self {
            _home: home,
            path,
            server,
        }
    }

    fn join(self) {
        self.server.join().unwrap();
    }
}

fn pane(num: u32) -> Pane {
    Pane {
        pane: format!("p01JDPANE0000000000000000{num:02}").into(),
        num,
        key: "01JDSESSION000000000000000".into(),
        pid: 4242,
        argv: vec!["/bin/sh".into()],
        cwd: "/home/user".into(),
        started_at: "2026-11-11T12:00:00Z".parse().unwrap(),
        state: PaneState::Running,
        exit: None,
        cols: 120,
        rows: 40,
        meta: Meta::new(),
        title: String::new(),
        last_output_at: None,
        attach_clients: 0,
        subscribers: 0,
    }
}

fn spawn_request() -> Spawn {
    Spawn {
        key: "01JDSESSION000000000000000".into(),
        argv: vec!["/bin/sh".into()],
        cwd: "/home/user".into(),
        env: Default::default(),
        size: None,
        meta: Meta::new(),
    }
}

#[test]
fn a_holder_with_no_common_version_is_a_clear_version_mismatch() {
    let newer = ProtoRange {
        min: SUPPORTED.max + 1,
        max: SUPPORTED.max + 2,
    };
    let holder = Holder::serve(vec![Box::new(move |mut peer: Peer| {
        assert_eq!(peer.handshake(newer), None);
    })]);
    let err = HolderClient::connect(&holder.path, "daemon").unwrap_err();
    let text = err.to_string();
    let PanesError::Version {
        client,
        holder: theirs,
    } = err
    else {
        panic!("expected a version mismatch, got {err:?}");
    };
    assert_eq!((client, theirs), (SUPPORTED, newer));
    assert!(text.contains("protocol version mismatch"), "{text}");
    assert!(
        text.contains(&format!("speaks {newer}")) && text.contains(&format!("speaks {SUPPORTED}")),
        "{text}"
    );
    assert!(text.contains("upgrade the older"), "{text}");
    holder.join();
}

#[test]
fn a_welcome_with_a_version_this_build_does_not_speak_is_a_mismatch() {
    let holder = Holder::serve(vec![Box::new(|mut peer: Peer| {
        let _ = peer.request();
        peer.send(&HolderMessage::Welcome(welcome(SUPPORTED.max + 1)));
    })]);
    let err = HolderClient::connect(&holder.path, "daemon").unwrap_err();
    assert!(
        matches!(err, PanesError::Version { holder: theirs, .. } if theirs.min == SUPPORTED.max + 1),
        "{err:?}"
    );
    holder.join();
}

#[test]
fn no_socket_is_unreachable() {
    let home = tempfile::tempdir().unwrap();
    let err = HolderClient::connect(socket_path(home.path()), "daemon").unwrap_err();
    assert!(matches!(err, PanesError::Unreachable(_)), "{err:?}");
}

#[test]
fn spawn_and_list_round_trip_and_an_error_reply_keeps_the_connection() {
    let holder = Holder::serve(vec![Box::new(|mut peer: Peer| {
        peer.handshake(SUPPORTED);
        let Request::Spawn { id, spawn } = peer.request() else {
            panic!("expected spawn");
        };
        assert_eq!(spawn, spawn_request());
        // A message type from a newer holder comes first, and is skipped.
        peer.send_frame(&Frame::Message(r#"{"type":"quiet","pane":"p1"}"#.into()));
        peer.send(&HolderMessage::Pane {
            id,
            pane: pane(1),
            existing: false,
        });
        let Request::Spawn { id, .. } = peer.request() else {
            panic!("expected spawn");
        };
        peer.send(&HolderMessage::Error {
            id: Some(id),
            code: ErrorCode::Busy,
            message: "128 panes".into(),
        });
        let Request::List { id } = peer.request() else {
            panic!("expected list");
        };
        peer.send(&HolderMessage::Panes {
            id,
            listing: Listing {
                instance: "01JDINSTANCE00000000000000".into(),
                panes: vec![pane(1)],
            },
        });
    })]);
    let client = HolderClient::connect(&holder.path, "daemon").unwrap();
    assert_eq!(client.welcome().instance, "01JDINSTANCE00000000000000");
    assert_eq!(client.spawn(&spawn_request()).unwrap(), pane(1));
    let err = client.spawn(&spawn_request()).unwrap_err();
    assert!(
        matches!(
            &err,
            PanesError::Refused {
                request: "spawn",
                code: ErrorCode::Busy,
                ..
            }
        ),
        "{err:?}"
    );
    let listing = client.list().unwrap();
    assert_eq!(listing.panes, vec![pane(1)]);
    holder.join();
}

#[test]
fn a_broken_reply_retires_the_connection() {
    let holder = Holder::serve(vec![Box::new(|mut peer: Peer| {
        peer.handshake(SUPPORTED);
        let _ = peer.request();
        // A message with no type is a protocol break.
        peer.send_frame(&Frame::Message("{}".into()));
    })]);
    let client = HolderClient::connect(&holder.path, "daemon").unwrap();
    let err = client.list().unwrap_err();
    assert!(matches!(err, PanesError::Protocol(_)), "{err:?}");
    let err = client.list().unwrap_err();
    assert!(matches!(err, PanesError::Unreachable(_)), "{err:?}");
    assert!(err.to_string().contains("connect again"), "{err}");
    holder.join();
}

#[test]
fn a_subscription_yields_the_repaint_then_output_and_raw_input_reaches_the_pane() {
    let pane_id: PaneId = pane(3).pane;
    let script_pane = pane_id.clone();
    let holder = Holder::serve(vec![
        // The request connection: raw input only.
        Box::new(|mut peer: Peer| {
            peer.handshake(SUPPORTED);
            assert_eq!(
                peer.frame(),
                Some(Frame::Input {
                    num: 3,
                    bytes: b"ls\r".to_vec()
                })
            );
            assert_eq!(peer.frame(), None);
        }),
        // The subscription's own connection.
        Box::new(move |mut peer: Peer| {
            peer.handshake(SUPPORTED);
            let Request::Subscribe { id, pane, sync } = peer.request() else {
                panic!("expected subscribe");
            };
            assert_eq!((pane, sync), (script_pane.clone(), SyncMode::Buffer));
            peer.send(&HolderMessage::Subscribed {
                id,
                stream: 9,
                cols: 120,
                rows: 40,
            });
            peer.send(&HolderMessage::Repaint {
                stream: 9,
                cols: 120,
                rows: 40,
            });
            peer.send_frame(&Frame::Output {
                stream: 9,
                bytes: b"\x1b[H\x1b[2J$ ".to_vec(),
            });
            peer.send_frame(&Frame::Output {
                stream: 9,
                bytes: b"ls\r\n".to_vec(),
            });
            let Request::Resize {
                id,
                pane,
                cols,
                rows,
                lease,
            } = peer.request()
            else {
                panic!("expected resize");
            };
            assert_eq!((pane, cols, rows), (script_pane.clone(), 100, 30));
            assert!(!lease.is_empty());
            peer.send(&HolderMessage::Ok {
                id,
                cols: Some(100),
                rows: Some(30),
            });
            peer.send(&HolderMessage::Exited {
                pane: script_pane,
                exit: Exit {
                    code: Some(0),
                    signal: None,
                    at: "2026-11-11T12:01:00Z".parse().unwrap(),
                },
            });
        }),
    ]);
    let client = HolderClient::connect(&holder.path, "daemon").unwrap();
    let mut subscription = client
        .subscribe(
            &pane_id,
            Subscribe {
                sync: SyncMode::Buffer,
            },
        )
        .unwrap();
    assert_eq!((subscription.stream(), subscription.size()), (9, (120, 40)));
    assert_eq!(
        subscription.recv().unwrap(),
        Some(Update::Repaint {
            cols: 120,
            rows: 40,
            bytes: b"\x1b[H\x1b[2J$ ".to_vec()
        })
    );
    client.raw_input(3, b"ls\r").unwrap();
    assert_eq!(
        subscription.recv().unwrap(),
        Some(Update::Output(b"ls\r\n".to_vec()))
    );
    // The resize goes from another thread while this one waits in recv.
    let resizer = subscription.resizer();
    let resizing = thread::spawn(move || resizer.resize(100, 30).unwrap());
    assert_eq!(
        subscription.recv().unwrap(),
        Some(Update::Resized {
            cols: 100,
            rows: 30
        })
    );
    resizing.join().unwrap();
    let Some(Update::Exited(exit)) = subscription.recv().unwrap() else {
        panic!("expected the exit");
    };
    assert_eq!(exit.code, Some(0));
    assert_eq!(subscription.recv().unwrap(), None);
    assert_eq!(subscription.recv().unwrap(), None);
    drop(client);
    holder.join();
}

#[test]
fn output_before_the_repaint_breaks_the_protocol() {
    let holder = Holder::serve(vec![
        Box::new(|mut peer: Peer| {
            peer.handshake(SUPPORTED);
            assert_eq!(peer.frame(), None);
        }),
        Box::new(|mut peer: Peer| {
            peer.handshake(SUPPORTED);
            let id = peer.request().id().unwrap();
            peer.send(&HolderMessage::Subscribed {
                id,
                stream: 1,
                cols: 80,
                rows: 24,
            });
            peer.send_frame(&Frame::Output {
                stream: 1,
                bytes: b"x".to_vec(),
            });
        }),
    ]);
    let client = HolderClient::connect(&holder.path, "daemon").unwrap();
    let mut subscription = client
        .subscribe(&"p1".into(), Subscribe::default())
        .unwrap();
    let err = subscription.recv().unwrap_err();
    assert!(matches!(err, PanesError::Protocol(_)), "{err:?}");
    assert_eq!(subscription.recv().unwrap(), None);
    drop(client);
    holder.join();
}

#[test]
fn subscribing_to_a_missing_pane_is_not_found() {
    let holder = Holder::serve(vec![
        Box::new(|mut peer: Peer| {
            peer.handshake(SUPPORTED);
            assert_eq!(peer.frame(), None);
        }),
        Box::new(|mut peer: Peer| {
            peer.handshake(SUPPORTED);
            let id = peer.request().id();
            peer.send(&HolderMessage::Error {
                id,
                code: ErrorCode::NotFound,
                message: "no pane p404".into(),
            });
        }),
    ]);
    let client = HolderClient::connect(&holder.path, "daemon").unwrap();
    let err = client
        .subscribe(&"p404".into(), Subscribe::default())
        .unwrap_err();
    assert!(matches!(err, PanesError::NotFound(_)), "{err:?}");
    drop(client);
    holder.join();
}

#[test]
fn an_error_with_no_id_answers_the_request_in_flight_and_keeps_the_connection() {
    let holder = Holder::serve(vec![Box::new(|mut peer: Peer| {
        peer.handshake(SUPPORTED);
        let _ = peer.request();
        // The spec's answer to a request type the holder does not know.
        peer.send_frame(&Frame::Message(
            r#"{"type":"error","code":"unsupported"}"#.into(),
        ));
        let Request::List { id } = peer.request() else {
            panic!("expected list");
        };
        peer.send(&HolderMessage::Panes {
            id,
            listing: Listing {
                instance: INSTANCE.into(),
                panes: Vec::new(),
            },
        });
    })]);
    let client = HolderClient::connect(&holder.path, "daemon").unwrap();
    let err = client.list().unwrap_err();
    assert!(
        matches!(
            &err,
            PanesError::Refused {
                code: ErrorCode::Unsupported,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(client.list().unwrap().panes.is_empty());
    holder.join();
}

/// Serves a request connection that only waits for the client to go, then
/// a subscription connection that runs `script` after `subscribed`.
fn subscription_holder(script: impl FnOnce(&mut Peer, u32) + Send + 'static) -> Holder {
    Holder::serve(vec![
        Box::new(|mut peer: Peer| {
            peer.handshake(SUPPORTED);
            assert_eq!(peer.frame(), None);
        }),
        Box::new(move |mut peer: Peer| {
            peer.handshake(SUPPORTED);
            let id = peer.request().id().unwrap();
            peer.send(&HolderMessage::Subscribed {
                id,
                stream: 5,
                cols: 80,
                rows: 24,
            });
            peer.send(&HolderMessage::Repaint {
                stream: 5,
                cols: 80,
                rows: 24,
            });
            peer.send_frame(&Frame::Output {
                stream: 5,
                bytes: b"$ ".to_vec(),
            });
            script(&mut peer, 5);
        }),
    ])
}

#[test]
fn a_lagged_subscriber_is_told_and_its_stream_ends() {
    let holder = subscription_holder(|peer, stream| {
        peer.send(&HolderMessage::Lagged { stream });
        // The holder may keep the connection open; the stream is over anyway.
        assert_eq!(peer.frame(), None);
    });
    let client = HolderClient::connect(&holder.path, "daemon").unwrap();
    let mut subscription = client
        .subscribe(&"p1".into(), Subscribe::default())
        .unwrap();
    assert!(matches!(
        subscription.recv(),
        Ok(Some(Update::Repaint { .. }))
    ));
    assert_eq!(subscription.recv().unwrap(), Some(Update::Lagged));
    assert_eq!(subscription.recv().unwrap(), None);
    drop(subscription);
    drop(client);
    holder.join();
}

#[test]
fn a_resize_refused_keeps_the_stream_and_an_ok_for_no_resize_breaks_it() {
    let holder = subscription_holder(|peer, _| {
        let id = peer.request().id();
        peer.send(&HolderMessage::Error {
            id,
            code: ErrorCode::BadRequest,
            message: "a size of 0".into(),
        });
        peer.send(&HolderMessage::Ok {
            id: 99,
            cols: Some(80),
            rows: Some(24),
        });
        assert_eq!(peer.frame(), None);
    });
    let client = HolderClient::connect(&holder.path, "daemon").unwrap();
    let mut subscription = client
        .subscribe(&"p1".into(), Subscribe::default())
        .unwrap();
    assert!(matches!(
        subscription.recv(),
        Ok(Some(Update::Repaint { .. }))
    ));
    subscription.resize(0, 0).unwrap();
    let err = subscription.recv().unwrap_err();
    assert!(
        matches!(
            &err,
            PanesError::Refused {
                request: "resize",
                code: ErrorCode::BadRequest,
                ..
            }
        ),
        "{err:?}"
    );
    let err = subscription.recv().unwrap_err();
    assert!(matches!(err, PanesError::Protocol(_)), "{err:?}");
    assert_eq!(subscription.recv().unwrap(), None);
    drop(subscription);
    drop(client);
    holder.join();
}

#[test]
fn dropping_a_subscription_ends_its_connection_while_a_resizer_lives() {
    let holder = subscription_holder(|peer, _| {
        assert_eq!(peer.frame(), None);
    });
    let client = HolderClient::connect(&holder.path, "daemon").unwrap();
    let subscription = client
        .subscribe(&"p1".into(), Subscribe::default())
        .unwrap();
    let resizer = subscription.resizer();
    drop(subscription);
    assert!(resizer.resize(100, 30).is_err());
    drop(client);
    holder.join();
}

#[test]
fn a_restarted_holder_on_subscribe_retires_the_client() {
    let holder = Holder::serve(vec![
        Box::new(|mut peer: Peer| {
            peer.handshake(SUPPORTED);
            assert_eq!(peer.frame(), None);
        }),
        Box::new(|mut peer: Peer| {
            let _ = peer.request();
            peer.send(&HolderMessage::Welcome(welcome_from(
                SUPPORTED.max,
                "01JDOTHERINSTANCE000000000",
            )));
        }),
    ]);
    let client = HolderClient::connect(&holder.path, "daemon").unwrap();
    let err = client
        .subscribe(&"p1".into(), Subscribe::default())
        .unwrap_err();
    assert!(matches!(&err, PanesError::Unreachable(_)), "{err:?}");
    assert!(err.to_string().contains("restarted"), "{err}");
    let err = client.list().unwrap_err();
    assert!(err.to_string().contains("connect again"), "{err}");
    drop(client);
    holder.join();
}

#[test]
fn a_socket_path_too_long_is_a_setup_error_not_unreachable() {
    let path = PathBuf::from(format!("/home/user/{}/run/holder.sock", "a".repeat(120)));
    let err = HolderClient::connect(&path, "daemon").unwrap_err();
    let PanesError::Io(io) = &err else {
        panic!("expected an io error, got {err:?}");
    };
    assert_eq!(io.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("TILLER_HOME"), "{err}");
}

#[test]
fn a_request_too_large_to_send_is_refused_and_keeps_the_connection() {
    let holder = Holder::serve(vec![Box::new(|mut peer: Peer| {
        peer.handshake(SUPPORTED);
        let Request::List { id } = peer.request() else {
            panic!("the oversized spawn reached the holder");
        };
        peer.send(&HolderMessage::Panes {
            id,
            listing: Listing {
                instance: INSTANCE.into(),
                panes: Vec::new(),
            },
        });
    })]);
    let client = HolderClient::connect(&holder.path, "daemon").unwrap();
    let mut spawn = spawn_request();
    spawn
        .meta
        .insert("huge".into(), "x".repeat(MAX_FRAME_LEN as usize));
    let err = client.spawn(&spawn).unwrap_err();
    assert!(
        matches!(
            &err,
            PanesError::Refused {
                request: "spawn",
                code: ErrorCode::BadRequest,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(client.list().unwrap().panes.is_empty());
    holder.join();
}

#[test]
fn raw_input_does_not_wait_for_a_call_in_flight() {
    let (asked, list_in_flight) = std::sync::mpsc::channel();
    let holder = Holder::serve(vec![Box::new(move |mut peer: Peer| {
        peer.handshake(SUPPORTED);
        let Request::List { id } = peer.request() else {
            panic!("expected list");
        };
        asked.send(()).unwrap();
        // The list gets no reply until the keystroke has come.
        assert_eq!(
            peer.frame(),
            Some(Frame::Input {
                num: 1,
                bytes: b"y".to_vec()
            })
        );
        peer.send(&HolderMessage::Panes {
            id,
            listing: Listing {
                instance: INSTANCE.into(),
                panes: Vec::new(),
            },
        });
    })]);
    let client = std::sync::Arc::new(HolderClient::connect(&holder.path, "daemon").unwrap());
    let listing = {
        let client = client.clone();
        thread::spawn(move || client.list())
    };
    list_in_flight.recv().unwrap();
    client.raw_input(1, b"y").unwrap();
    assert!(listing.join().unwrap().unwrap().panes.is_empty());
    drop(client);
    holder.join();
}

#[test]
fn a_retired_client_subscribes_to_nothing() {
    let holder = Holder::serve(vec![Box::new(|mut peer: Peer| {
        peer.handshake(SUPPORTED);
        let _ = peer.request();
        peer.send_frame(&Frame::Message("{}".into()));
    })]);
    let client = HolderClient::connect(&holder.path, "daemon").unwrap();
    assert!(client.list().is_err());
    let err = client
        .subscribe(&"p1".into(), Subscribe::default())
        .unwrap_err();
    assert!(err.to_string().contains("connect again"), "{err}");
    holder.join();
}
