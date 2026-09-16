use crate::protocol::*;
use std::io;

#[test]
fn capability_probe_requires_complete_compatible_service() {
    let mut status = Capabilities {
        version: VERSION,
        service: "codex-freebsd-sandboxd".into(),
        features: crate::REQUIRED_CAPABILITIES
            .iter()
            .map(|value| (*value).into())
            .collect(),
    };
    assert!(crate::validate_capabilities(&status).is_ok());
    status.features.pop();
    assert!(crate::validate_capabilities(&status).is_err());
    status.version += 1;
    assert!(crate::validate_capabilities(&status).is_err());
}

#[test]
fn concrete_policy_rejects_root_and_relative_grants() {
    use crate::policy::Access;
    use crate::policy::Grant;
    use crate::policy::Network;
    use crate::policy::Policy;
    for path in ["/", "relative", "/work/../private"] {
        let policy = Policy {
            grants: vec![Grant {
                path: path.into(),
                access: Access::Read,
            }],
            network: Network::Restricted,
        };
        assert!(policy.permissions().is_err(), "{path}");
    }
    let policy = Policy {
        grants: vec![Grant {
            path: "/work".into(),
            access: Access::Write,
        }],
        network: Network::Restricted,
    };
    assert_eq!(
        policy.permissions().unwrap()["file_system"]["entries"][1]["access"],
        "write"
    );
}

#[test]
fn launch_debug_does_not_disclose_secrets() {
    let request = Launch {
        version: VERSION,
        argv: vec!["secret-command".into()],
        cwd: "/private/path".into(),
        policy_cwd: "/private/path".into(),
        permissions: serde_json::json!({}),
        env: [("TOKEN".into(), "secret-value".into())].into(),
        terminal: None,
    };
    let debug = format!("{request:?}");
    assert!(!debug.contains("secret"));
    assert!(!debug.contains("/private"));
}

#[test]
fn preserves_binary_input_and_frame_boundaries() -> io::Result<()> {
    let mut wire = Vec::new();
    send(
        &mut wire,
        &Request::Input {
            data: vec![0, 255, 10],
        },
    )?;
    send(&mut wire, &Request::Eof)?;
    let mut reader = wire.as_slice();
    let Request::Input { data } = receive(&mut reader)? else {
        panic!("input frame");
    };
    assert_eq!(data, vec![0, 255, 10]);
    assert!(matches!(receive::<Request>(&mut reader)?, Request::Eof));
    assert!(reader.is_empty());
    Ok(())
}

#[test]
fn rejects_oversized_frames_before_reading_payload() {
    let bytes = (MAX_FRAME as u32 + 1).to_be_bytes();
    let error = receive::<Request>(&mut bytes.as_slice()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn rejects_forged_identity_fields() {
    assert!(serde_json::from_str::<Request>(r#"{"type":"Probe","version":1,"uid":0}"#).is_err());
}

#[test]
fn rejects_truncated_payload() {
    let mut bytes = 100u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(b"{}");
    assert_eq!(
        receive::<Request>(&mut bytes.as_slice())
            .unwrap_err()
            .kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn streams_status_binary_output_and_exit() -> io::Result<()> {
    let mut wire = Vec::new();
    send(
        &mut wire,
        &Response::Status {
            version: VERSION,
            description: "test service".into(),
        },
    )?;
    send(
        &mut wire,
        &Response::Output {
            stderr: true,
            data: vec![255; CHUNK],
        },
    )?;
    send(&mut wire, &Response::Exit { code: 42 })?;
    let mut reader = wire.as_slice();
    assert!(matches!(
        receive::<Response>(&mut reader)?,
        Response::Status {
            version: VERSION,
            ..
        }
    ));
    let Response::Output { stderr, data } = receive(&mut reader)? else {
        panic!("output frame")
    };
    assert_eq!((stderr, data), (true, vec![255; CHUNK]));
    assert!(matches!(
        receive::<Response>(&mut reader)?,
        Response::Exit { code: 42 }
    ));
    assert!(reader.is_empty());
    Ok(())
}
