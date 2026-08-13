// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com> This program is free
// software: you can redistribute it and/or modify it under the terms of the GNU
// Affero General Public License as published by the Free Software Foundation,
// version 3.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
// FOR A PARTICULAR PURPOSE. See the GNU Affero General Public License for more
// details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use super::*;
use crate::{HostMessage, MAGIC};
use mica_var::{Identity, Symbol, Tuple, Value};

fn id(raw: u64) -> Identity {
    Identity::new(raw).unwrap()
}

#[test]
fn hello_frame_matches_golden_bytes() {
    let message = HostMessage::Hello {
        protocol_version: 1,
        min_protocol_version: 1,
        feature_bits: 0,
        host_name: "h".to_owned(),
    };
    let frame = encoded_frame(&message).unwrap();
    assert_eq!(
        frame,
        vec![
            b'M', b'H', b'P', b'1', 21, 0, 0, 0, 1, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
            0, 0, 0, b'h',
        ]
    );
    assert_eq!(decode_frame(&frame).unwrap(), message);
}

#[test]
fn submit_input_frame_matches_golden_bytes() {
    let message = HostMessage::SubmitInput {
        request_id: 9,
        endpoint: id(42),
        value: Value::int(7).unwrap(),
    };
    let frame = encoded_frame(&message).unwrap();
    assert_eq!(
        frame,
        vec![
            b'M', b'H', b'P', b'1', 28, 0, 0, 0, 1, 2, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 42, 0, 0, 0,
            0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 2,
        ]
    );
    assert_eq!(decode_frame(&frame).unwrap(), message);
}

#[test]
fn segmented_encoder_matches_contiguous_encoder_and_borrows_payloads() {
    let message = HostMessage::SubmitSource {
        request_id: 1,
        endpoint: id(1),
        actor: id(2),
        source: "look north".to_owned(),
    };
    let contiguous = encoded_frame(&message).unwrap();
    let segments = encode_frame_segments(&message).unwrap();

    assert_eq!(segments.to_vec(), contiguous);
    assert_eq!(segments.len(), contiguous.len());
    assert!(segments.io_slices().len() > 1);

    let borrowed_source = segments
        .segments
        .iter()
        .filter_map(|&segment| match segment {
            FrameSegment::Borrowed(bytes) => Some(bytes),
            _ => None,
        })
        .any(|bytes| bytes == b"look north");
    assert!(borrowed_source);
}

#[test]
fn segmented_encoder_uses_value_segments_for_heap_payloads() {
    let message = HostMessage::SubmitInput {
        request_id: 1,
        endpoint: id(42),
        value: Value::bytes(b"payload"),
    };
    let contiguous = encoded_frame(&message).unwrap();
    let segments = encode_frame_segments(&message).unwrap();

    assert_eq!(segments.to_vec(), contiguous);
    let borrowed_value_payload = segments
        .segments
        .iter()
        .filter_map(|&segment| match segment {
            FrameSegment::ValueBorrowed { .. } => Some(segments.segment_bytes(segment)),
            _ => None,
        })
        .any(|bytes| bytes == b"payload");
    assert!(borrowed_value_payload);
}

#[test]
fn task_completion_segments_round_trip_relation_values() {
    let relation = Value::relation(
        [Symbol::intern("payload")],
        [Tuple::from([Value::string("relation payload")])],
    )
    .unwrap();
    let message = HostMessage::TaskCompleted {
        task_id: 7,
        value: relation,
    };

    let contiguous = encoded_frame(&message).unwrap();
    let segments = encode_frame_segments(&message).unwrap();

    assert_eq!(segments.to_vec(), contiguous);
    assert_eq!(decode_frame(&contiguous).unwrap(), message);
    assert!(
        segments
            .segments
            .iter()
            .filter_map(|&segment| match segment {
                FrameSegment::ValueBorrowed { .. } => Some(segments.segment_bytes(segment)),
                _ => None,
            })
            .any(|bytes| bytes == b"relation payload")
    );
}

#[test]
fn task_completion_frames_round_trip_standard_relational_values() {
    let values = [
        Value::unit(),
        Value::relation([Symbol::intern("value")], []).unwrap(),
        Value::relation(
            [Symbol::intern("case"), Symbol::intern("value")],
            [Tuple::from([
                Value::symbol(Symbol::intern("error")),
                Value::error(Symbol::intern("E_SAMPLE"), Some("sample"), None),
            ])],
        )
        .unwrap(),
    ];

    for (task_id, value) in values.into_iter().enumerate() {
        let message = HostMessage::TaskCompleted {
            task_id: task_id as u64,
            value,
        };
        assert_eq!(
            decode_frame(&encoded_frame(&message).unwrap()).unwrap(),
            message
        );
    }
}

#[test]
fn stream_decoder_waits_for_split_frames() {
    let message = HostMessage::OpenEndpoint {
        request_id: 1,
        endpoint: id(1),
        actor: None,
        protocol: "telnet".to_owned(),
        grant_token: Some("grant".to_owned()),
    };
    let frame = encoded_frame(&message).unwrap();
    let mut decoder = FrameDecoder::default();

    decoder.push_bytes(&frame[..5]);
    assert_eq!(decoder.peek_frame().unwrap(), None);

    decoder.push_bytes(&frame[5..]);
    let borrowed = decoder.peek_frame().unwrap().unwrap();
    assert_eq!(borrowed.message_type().unwrap(), MessageType::OpenEndpoint);
    assert_eq!(borrowed.decode().unwrap(), message);
    assert!(decoder.consume_frame().unwrap());
    assert!(decoder.is_empty());
}

#[test]
fn stream_decoder_handles_multiple_frames_in_one_read() {
    let first = HostMessage::CloseEndpoint {
        request_id: 1,
        endpoint: id(1),
    };
    let second = HostMessage::DrainOutput {
        request_id: 2,
        endpoint: id(1),
        limit: 32,
    };
    let mut bytes = encoded_frame(&first).unwrap();
    bytes.extend_from_slice(&encoded_frame(&second).unwrap());

    let mut decoder = FrameDecoder::default();
    decoder.push_bytes(&bytes);

    assert_eq!(
        decoder.peek_frame().unwrap().unwrap().decode().unwrap(),
        first
    );
    assert!(decoder.consume_frame().unwrap());
    assert_eq!(
        decoder.peek_frame().unwrap().unwrap().decode().unwrap(),
        second
    );
    assert!(decoder.consume_frame().unwrap());
    assert!(!decoder.consume_frame().unwrap());
    assert!(decoder.is_empty());
}

#[test]
fn stream_decoder_rejects_oversized_frames() {
    let message = HostMessage::CloseEndpoint {
        request_id: 1,
        endpoint: id(1),
    };
    let frame = encoded_frame(&message).unwrap();
    let mut decoder = FrameDecoder::new(4);
    decoder.push_bytes(&frame);

    assert_eq!(
        decoder.peek_frame(),
        Err(HostProtocolError::FrameTooLarge(frame.len() - 8))
    );
}

#[test]
fn round_trips_endpoint_task_and_output_messages() {
    let messages = [
        HostMessage::HelloAck {
            protocol_version: 1,
            feature_bits: 7,
        },
        HostMessage::RequestAccepted {
            request_id: 1,
            task_id: Some(10),
        },
        HostMessage::RequestAccepted {
            request_id: 2,
            task_id: None,
        },
        HostMessage::RequestRejected {
            request_id: 3,
            code: Symbol::intern("E_DENIED"),
            message: "no".to_owned(),
        },
        HostMessage::OpenEndpoint {
            request_id: 4,
            endpoint: id(1),
            actor: None,
            protocol: "telnet".to_owned(),
            grant_token: None,
        },
        HostMessage::CloseEndpoint {
            request_id: 5,
            endpoint: id(1),
        },
        HostMessage::ResolveIdentity {
            request_id: 6,
            name: Symbol::intern("alice"),
        },
        HostMessage::IdentityResolved {
            request_id: 7,
            name: Symbol::intern("alice"),
            identity: id(2),
        },
        HostMessage::SubmitSource {
            request_id: 8,
            endpoint: id(1),
            actor: id(2),
            source: "emit(#1, \"hi\")".to_owned(),
        },
        HostMessage::OutputReady {
            endpoint: id(1),
            buffered: 3,
        },
        HostMessage::DrainOutput {
            request_id: 9,
            endpoint: id(1),
            limit: 64,
        },
        HostMessage::OutputBatch {
            endpoint: id(1),
            values: vec![
                Value::string("hello"),
                Value::symbol(Symbol::intern("ready")),
            ],
        },
        HostMessage::EndpointClosed {
            endpoint: id(1),
            reason: "closed".to_owned(),
        },
        HostMessage::TaskCompleted {
            task_id: 10,
            value: Value::relation(
                [Symbol::intern("name"), Symbol::intern("score")],
                [Tuple::from([
                    Value::string("Ada"),
                    Value::float(9.5).unwrap(),
                ])],
            )
            .unwrap(),
        },
        HostMessage::TaskFailed {
            task_id: 11,
            error: Value::error(
                Symbol::intern("E_TEST"),
                Some("failed"),
                Some(Value::int(5).unwrap()),
            ),
        },
    ];

    for message in messages {
        let frame = encoded_frame(&message).unwrap();
        assert_eq!(decode_frame(&frame).unwrap(), message);
    }
}

#[test]
fn rejects_unknown_message_type() {
    let mut frame = Vec::new();
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&4u32.to_le_bytes());
    frame.extend_from_slice(&0xffffu16.to_le_bytes());
    frame.extend_from_slice(&0u16.to_le_bytes());
    assert_eq!(
        decode_frame(&frame),
        Err(HostProtocolError::UnknownMessageType(0xffff))
    );
}

#[test]
fn rejects_bad_magic_and_bad_frame_length() {
    let message = HostMessage::CloseEndpoint {
        request_id: 1,
        endpoint: id(1),
    };
    let mut frame = encoded_frame(&message).unwrap();
    frame[0] = b'X';
    assert_eq!(
        decode_frame(&frame),
        Err(HostProtocolError::InvalidMagic(*b"XHP1"))
    );

    let mut frame = encoded_frame(&message).unwrap();
    frame[4..8].copy_from_slice(&99u32.to_le_bytes());
    assert!(matches!(
        decode_frame(&frame),
        Err(HostProtocolError::UnexpectedEnd { .. })
    ));
}

#[test]
fn rejects_reserved_flags_and_trailing_frame_bytes() {
    let message = HostMessage::CloseEndpoint {
        request_id: 1,
        endpoint: id(1),
    };
    let mut frame = encoded_frame(&message).unwrap();
    frame[10..12].copy_from_slice(&1u16.to_le_bytes());
    assert_eq!(
        decode_frame(&frame),
        Err(HostProtocolError::UnsupportedFlags(1))
    );

    let mut frame = encoded_frame(&message).unwrap();
    frame.extend_from_slice(&[0]);
    assert_eq!(
        decode_frame(&frame),
        Err(HostProtocolError::TrailingFrameBytes(1))
    );
}
