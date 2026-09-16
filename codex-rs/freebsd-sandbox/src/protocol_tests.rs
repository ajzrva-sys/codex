use super::*;

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
