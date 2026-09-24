use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};

struct Lab(PathBuf);
impl Lab {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(std::env::temp_dir().join(format!(
            "ew-journal-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct IntermittentInput(std::collections::VecDeque<io::Result<Vec<u8>>>);

impl Read for IntermittentInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let bytes = self
            .0
            .pop_front()
            .expect("Unexpected additional recorder read")?;
        buffer[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
}

#[test]
fn finalization_drains_pending_bytes_before_closing() {
    let lab = Lab::new();
    let mut recorder = Recorder::create(&lab.0).unwrap();
    request_finalization(&lab.0).unwrap();
    request_finalization(&lab.0).unwrap();
    let mut input = IntermittentInput(
        [
            Ok(b"first\r\n".to_vec()),
            Err(io::ErrorKind::Interrupted.into()),
            Ok(b"last\xff\x00".to_vec()),
            Err(io::ErrorKind::WouldBlock.into()),
        ]
        .into(),
    );
    drain_input(&mut recorder, &mut input).unwrap();
    assert!(!read(&lab.0, 0, MAX_PAGE_BYTES).unwrap().closed);
    recorder.close().unwrap();
    let page = read(&lab.0, 0, MAX_PAGE_BYTES).unwrap();
    assert!(page.closed);
    assert_eq!(page.bytes, b"first\r\nlast\xff\x00");
    assert_eq!(
        fs::metadata(lab.0.join("finalize.request"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn quiet_socket_is_not_mistaken_for_shell_exit() {
    let lab = Lab::new();
    let mut recorder = Recorder::create(&lab.0).unwrap();
    let mut input = IntermittentInput(
        [
            Err(io::ErrorKind::TimedOut.into()),
            Err(io::ErrorKind::WouldBlock.into()),
            Ok(b"still running".to_vec()),
            Ok(Vec::new()),
        ]
        .into(),
    );
    drain_input(&mut recorder, &mut input).unwrap();
    recorder.close().unwrap();
    assert_eq!(
        read(&lab.0, 0, MAX_PAGE_BYTES).unwrap().bytes,
        b"still running"
    );
}

#[test]
fn finalization_request_rejects_symlinks() {
    let lab = Lab::new();
    let _recorder = Recorder::create(&lab.0).unwrap();
    std::os::unix::fs::symlink(lab.0.join("manifest"), lab.0.join("finalize.request")).unwrap();
    assert!(request_finalization(&lab.0).is_err());
    assert!(finalization_requested(&lab.0).is_err());
}

#[test]
fn raw_shell_hooks_and_non_utf8_output_survive_paging() {
    let lab = Lab::new();
    let mut recorder = Recorder::with_retention(&lab.0, 16, 8).unwrap();
    let expected = b"\x1b]9278;f;{\"hook\":\"Precmd\"}\x07\r\n\xff\x00\x1b[31mred\x1b[0m";
    recorder.append(expected).unwrap();
    recorder.close().unwrap();
    let mut actual = Vec::new();
    let mut cursor = 0;
    loop {
        let page = read(&lab.0, cursor, 7).unwrap();
        assert!(!page.history_gap);
        assert!(page.closed);
        assert_eq!(page.start_cursor, cursor);
        cursor = page.next_cursor;
        actual.extend(page.bytes);
        if cursor == page.high_watermark {
            break;
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn reconnect_at_cursor_neither_duplicates_nor_loses_live_output() {
    let lab = Lab::new();
    let mut recorder = Recorder::create(&lab.0).unwrap();
    recorder.append(b"before disconnect\n").unwrap();
    let first = read(&lab.0, 0, MAX_PAGE_BYTES).unwrap();
    recorder.append(b"offline\n").unwrap();
    let second = read(&lab.0, first.next_cursor, MAX_PAGE_BYTES).unwrap();
    assert_eq!(second.bytes, b"offline\n");
    assert_eq!(
        read(&lab.0, first.next_cursor, MAX_PAGE_BYTES).unwrap(),
        second
    );
    recorder.append(b"live again\n").unwrap();
    assert_eq!(
        read(&lab.0, second.next_cursor, MAX_PAGE_BYTES)
            .unwrap()
            .bytes,
        b"live again\n"
    );
}

#[test]
fn bounded_retention_explicitly_reports_history_gaps() {
    let lab = Lab::new();
    let mut recorder = Recorder::with_retention(&lab.0, 8, 2).unwrap();
    recorder.append(b"0000000011111111222222223333").unwrap();
    let page = read(&lab.0, 0, MAX_PAGE_BYTES).unwrap();
    assert!(page.history_gap);
    assert_eq!(page.earliest_cursor, 16);
    assert_eq!(page.start_cursor, 16);
    assert_eq!(page.next_cursor, 28);
    assert_eq!(page.bytes, b"222222223333");
    assert!(!segment_path(&lab.0, 0).exists());
    assert!(!segment_path(&lab.0, 1).exists());
}

#[test]
fn an_unpublished_tail_is_never_replayed() {
    let lab = Lab::new();
    let mut recorder = Recorder::create(&lab.0).unwrap();
    recorder.append(b"committed").unwrap();
    let mut file = OpenOptions::new()
        .append(true)
        .open(segment_path(&lab.0, 0))
        .unwrap();
    file.write_all(b"uncommitted").unwrap();
    assert_eq!(read(&lab.0, 0, MAX_PAGE_BYTES).unwrap().bytes, b"committed");
}

#[test]
fn recorder_cannot_restart_over_existing_incarnation() {
    let lab = Lab::new();
    let recorder = Recorder::create(&lab.0).unwrap();
    drop(recorder);
    assert!(Recorder::create(&lab.0).is_err());
    assert!(!read(&lab.0, 0, 1).unwrap().closed);
}

#[test]
fn rejects_future_cursors_bad_limits_and_corrupt_manifest() {
    let lab = Lab::new();
    let _recorder = Recorder::create(&lab.0).unwrap();
    assert!(read(&lab.0, 1, 1).is_err());
    assert!(read(&lab.0, 0, 0).is_err());
    assert!(read(&lab.0, 0, MAX_PAGE_BYTES + 1).is_err());
    fs::write(lab.0.join("manifest"), "EWJ1 0 64 0 0 0").unwrap();
    assert!(read(&lab.0, 0, 1).is_err());
}

#[test]
fn output_and_metadata_are_private() {
    let lab = Lab::new();
    let mut recorder = Recorder::create(&lab.0).unwrap();
    recorder.append(b"secret").unwrap();
    assert_eq!(
        fs::metadata(&lab.0).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for path in [lab.0.join("manifest"), segment_path(&lab.0, 0)] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn concurrent_reader_sees_consistent_prefixes_during_rotation() {
    let lab = Lab::new();
    let mut recorder = Recorder::with_retention(&lab.0, 32, 2).unwrap();
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut cursor = 0;
            loop {
                match read(&lab.0, cursor, 9) {
                    Ok(page) => {
                        for (index, byte) in page.bytes.iter().enumerate() {
                            assert_eq!(*byte, ((page.start_cursor + index as u64) % 251) as u8);
                        }
                        cursor = page.next_cursor;
                        if page.closed && cursor == page.high_watermark {
                            return cursor;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("{error}"),
                }
                std::thread::yield_now();
            }
        });
        for index in 0..128u64 {
            recorder.append(&[(index % 251) as u8]).unwrap();
        }
        recorder.close().unwrap();
        assert_eq!(reader.join().unwrap(), 128);
    });
}

#[test]
fn compressed_history_preserves_every_byte_after_hot_retention_expires() {
    let lab = Lab::new();
    let mut recorder = Recorder::with_configuration(&lab.0, 16, 2, true).unwrap();
    let expected: Vec<u8> = (0..127).map(|index| (index * 13) as u8).collect();
    recorder.append(&expected).unwrap();
    recorder.close().unwrap();
    assert!(!segment_path(&lab.0, 0).exists());
    assert!(archive_path(&lab.0, 0).is_file());
    assert_eq!(
        fs::metadata(archive_path(&lab.0, 0))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let mut actual = Vec::new();
    let mut cursor = 0;
    while cursor < expected.len() as u64 {
        let page = read(&lab.0, cursor, 7).unwrap();
        assert_eq!(page.earliest_cursor, 0);
        assert!(!page.history_gap);
        assert!(page.closed);
        cursor = page.next_cursor;
        actual.extend(page.bytes);
    }
    assert_eq!(actual, expected);
    let actual_storage: u64 = fs::read_dir(&lab.0)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_name().to_string_lossy().contains(".output"))
        .map(|entry| entry.metadata().unwrap().len())
        .sum();
    assert_eq!(retained_storage_bytes(&lab.0).unwrap(), actual_storage);
}

#[test]
fn compressed_history_rejects_corruption_and_excessive_expansion() {
    for oversized in [false, true] {
        let lab = Lab::new();
        let mut recorder = Recorder::with_configuration(&lab.0, 16, 1, true).unwrap();
        recorder.append(&[42; 48]).unwrap();
        recorder.close().unwrap();
        let path = archive_path(&lab.0, 0);
        if oversized {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(&[42; 17]).unwrap();
            fs::write(path, encoder.finish().unwrap()).unwrap();
        } else {
            fs::write(path, b"not a gzip stream").unwrap();
        }
        assert!(read(&lab.0, 0, MAX_PAGE_BYTES).is_err());
    }
}

#[test]
fn default_retention_archives_but_rolling_policy_remains_readable() {
    for policy in [
        RetentionPolicy::UntilWorkspaceDeleted,
        RetentionPolicy::Rolling,
    ] {
        let lab = Lab::new();
        let mut recorder = Recorder::create_with_policy(&lab.0, policy).unwrap();
        recorder.append(b"retained").unwrap();
        recorder.close().unwrap();
        assert_eq!(
            manifest(&lab.0).unwrap().archive,
            policy == RetentionPolicy::UntilWorkspaceDeleted
        );
        assert_eq!(read(&lab.0, 0, MAX_PAGE_BYTES).unwrap().bytes, b"retained");
    }
    assert_eq!(
        RetentionPolicy::default(),
        RetentionPolicy::UntilWorkspaceDeleted
    );
}

#[test]
fn readers_cross_hot_to_compressed_rotation_without_a_gap() {
    let lab = Lab::new();
    let mut recorder = Recorder::with_configuration(&lab.0, 16, 2, true).unwrap();
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut cursor = 0;
            loop {
                let page = read(&lab.0, cursor, 9).unwrap();
                assert!(!page.history_gap);
                assert_eq!(page.start_cursor, cursor);
                for (offset, byte) in page.bytes.iter().enumerate() {
                    assert_eq!(*byte, ((cursor + offset as u64) % 251) as u8);
                }
                cursor = page.next_cursor;
                if page.closed && cursor == page.high_watermark {
                    return cursor;
                }
                std::thread::yield_now();
            }
        });
        for index in 0..128u64 {
            recorder.append(&[(index % 251) as u8]).unwrap();
        }
        recorder.close().unwrap();
        assert_eq!(reader.join().unwrap(), 128);
    });
}
