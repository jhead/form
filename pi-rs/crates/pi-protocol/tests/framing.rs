//! Port of `.upstream/packages/protocol/test/framing.test.ts`.

use pi_protocol::{
    assert_complete_frame, encode_frame, FrameDecoder, FrameDecoderOptions, FrameError,
};

fn frame(payload: &[u8]) -> Vec<u8> {
    encode_frame(payload).expect("frames")
}

#[test]
fn prefixes_payloads_with_a_four_byte_big_endian_length() {
    assert_eq!(
        frame(&[0xaa, 0xbb, 0xcc]),
        vec![0x00, 0x00, 0x00, 0x03, 0xaa, 0xbb, 0xcc]
    );
    assert_eq!(frame(&[]), vec![0, 0, 0, 0]);
}

#[test]
fn validates_one_complete_bounded_frame() {
    assert_eq!(
        assert_complete_frame(
            &[0, 0, 0, 2, 1, 2],
            FrameDecoderOptions::with_max_frame_length(2)
        ),
        Ok(())
    );
    assert_eq!(
        assert_complete_frame(&[0, 0, 0, 2, 1], FrameDecoderOptions::default()),
        Err(FrameError::NotExactlyOnePayload)
    );
    assert_eq!(
        assert_complete_frame(&[0, 0, 0], FrameDecoderOptions::default()),
        Err(FrameError::IncompleteLengthPrefix)
    );
    assert_eq!(
        assert_complete_frame(&[0, 0, 0, 1, 1, 2], FrameDecoderOptions::default()),
        Err(FrameError::NotExactlyOnePayload)
    );
    assert_eq!(
        assert_complete_frame(
            &[0, 0, 0, 3, 1, 2, 3],
            FrameDecoderOptions::with_max_frame_length(2)
        ),
        Err(FrameError::LengthLimit {
            length: 3,
            limit: 2
        })
    );
}

#[test]
fn decodes_fragmented_coalesced_and_empty_frames_in_order() {
    let mut wire = frame(&[1, 2, 3]);
    wire.extend(frame(&[]));
    wire.extend(frame(&[4]));

    let mut decoder = FrameDecoder::new();
    let mut frames = Vec::new();
    for byte in &wire {
        frames.extend(decoder.push(&[*byte]).expect("pushes"));
    }
    decoder.end().expect("ends");
    assert_eq!(frames, vec![vec![1, 2, 3], vec![], vec![4]]);

    let mut coalesced = FrameDecoder::new();
    assert_eq!(coalesced.push(&wire).expect("pushes"), frames);
    coalesced.end().expect("ends");
}

#[test]
fn assembles_payloads_spanning_multiple_internal_blocks() {
    let payload: Vec<u8> = (0..70_000).map(|index| (index % 251) as u8).collect();
    let wire = frame(&payload);
    let mut decoder = FrameDecoder::new();
    let mut frames = decoder.push(&wire[0..101]).expect("pushes");
    frames.extend(decoder.push(&wire[101..65_541]).expect("pushes"));
    frames.extend(decoder.push(&wire[65_541..]).expect("pushes"));
    decoder.end().expect("ends");
    assert_eq!(frames, vec![payload]);
}

#[test]
fn handles_every_split_point_across_a_frame() {
    let wire = frame(&[10, 20, 30, 40]);
    for split in 0..=wire.len() {
        let mut decoder = FrameDecoder::new();
        let mut frames = decoder.push(&wire[..split]).expect("pushes");
        frames.extend(decoder.push(&wire[split..]).expect("pushes"));
        decoder.end().expect("ends");
        assert_eq!(frames, vec![vec![10, 20, 30, 40]], "split at {split}");
    }
}

#[test]
fn copies_payload_bytes_instead_of_aliasing_the_input() {
    let mut chunk = frame(&[1, 2, 3]);
    let mut decoder = FrameDecoder::new();
    let frames = decoder.push(&chunk).expect("pushes");
    chunk.fill(9);
    assert_eq!(frames, vec![vec![1, 2, 3]]);
}

#[test]
fn accepts_empty_chunks_and_a_clean_empty_stream() {
    let mut decoder = FrameDecoder::new();
    assert_eq!(decoder.push(&[]).expect("pushes"), Vec::<Vec<u8>>::new());
    decoder.end().expect("ends");
}

#[test]
fn rejects_a_truncated_stream_at_end() {
    for (label, wire) in [
        ("partial header", vec![0u8, 0, 0]),
        ("partial payload", vec![0, 0, 0, 2, 1]),
    ] {
        let mut decoder = FrameDecoder::new();
        assert_eq!(
            decoder.push(&wire).expect("pushes"),
            Vec::<Vec<u8>>::new(),
            "{label}"
        );
        assert_eq!(decoder.end(), Err(FrameError::TruncatedStream), "{label}");
        // Once failed the decoder stays failed.
        assert_eq!(decoder.end(), Err(FrameError::Failed), "{label}");
    }
}

#[test]
fn rejects_an_oversized_declared_length_as_soon_as_the_header_completes() {
    let mut decoder = FrameDecoder::with_options(FrameDecoderOptions::with_max_frame_length(3));
    assert_eq!(
        decoder.push(&[0, 0, 0, 4]),
        Err(FrameError::LengthLimit {
            length: 4,
            limit: 3
        })
    );
    assert_eq!(decoder.push(&[1]), Err(FrameError::Failed));
}

#[test]
fn accepts_a_frame_exactly_at_the_configured_maximum() {
    let mut decoder = FrameDecoder::with_options(FrameDecoderOptions::with_max_frame_length(3));
    assert_eq!(
        decoder.push(&frame(&[1, 2, 3])).expect("pushes"),
        vec![vec![1, 2, 3]]
    );
    decoder.end().expect("ends");
}

#[test]
fn cannot_be_pushed_after_end() {
    let mut decoder = FrameDecoder::new();
    decoder.end().expect("ends");
    assert_eq!(decoder.push(&[]), Err(FrameError::Ended));
    assert_eq!(decoder.end(), Err(FrameError::Ended));
}

#[test]
fn splits_a_chunk_carrying_several_frames_and_a_partial_tail() {
    let mut wire = frame(&[1]);
    wire.extend(frame(&[2, 2]));
    wire.extend(frame(&[3, 3, 3]));
    wire.truncate(wire.len() - 1);

    let mut decoder = FrameDecoder::new();
    assert_eq!(
        decoder.push(&wire).expect("pushes"),
        vec![vec![1], vec![2, 2]]
    );
    assert_eq!(decoder.push(&[3]).expect("pushes"), vec![vec![3, 3, 3]]);
    decoder.end().expect("ends");
}

// Upstream's "rejects invalid maximum frame length" case (-1, 1.5, NaN, and a
// value above 2^32-1) has no analogue here: `max_frame_length` is a `u32`, so
// none of those values can be constructed.
