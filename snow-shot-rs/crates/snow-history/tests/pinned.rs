//! 贴图仓储测试：移植自 C++ `pinned_window_repository_tests.cpp` 的容器层用例，外加崩溃一致性。

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use common::{TempDir, new_uuid};
use serde_json::{Map, Value, json};
use snow_history::pinned::{
    MAX_PAYLOAD_BYTES, PinCrashPoint, PinError, PinGroup, PinImage, PinOptions, PinPayload,
    PinnedStore,
};

/// 构造图片型记录的清单对象。
fn record(id: &str, kind: &str) -> Map<String, Value> {
    json!({"id": id, "group_id": "default", "source_kind": kind, "opacity_percent": 100})
        .as_object()
        .unwrap()
        .clone()
}

/// 构造带 canvas_session 的完整 payload。
fn payload(seed: u8) -> PinPayload {
    PinPayload {
        image: Some(PinImage {
            file_name: "source.png".into(),
            bytes: vec![seed; 64],
        }),
        original_html: "<p>original</p>".into(),
        original_text: "original".into(),
        result_style: b"style".to_vec(),
        canvas_session: format!("{{\"session\":{seed}}}").into_bytes(),
        recognition_results: b"recognition".to_vec(),
    }
}

/// 某贴图的 payload 目录。
fn pin_dir(root: &Path, id: &str) -> PathBuf {
    root.join("pinned_windows_v2").join("pins").join(id)
}

/// 清单路径。
fn manifest_path(root: &Path) -> PathBuf {
    root.join("pinned_windows_v2").join("index.json")
}

/// 读取清单。
fn read_manifest(root: &Path) -> Value {
    serde_json::from_slice(&fs::read(manifest_path(root)).unwrap()).unwrap()
}

/// 提交后 payload 从磁盘提供，重开后仍一致（committedPayloadsAreServedFromDisk）。
#[test]
fn committed_payloads_are_served_from_disk() {
    let dir = TempDir::new("pin-served");
    let id = new_uuid();
    let original = payload(5);
    {
        let mut store = PinnedStore::open(dir.path(), PinOptions::default());
        store
            .upsert(record(&id, "image_data"), Some(original.clone()))
            .unwrap();
        assert!(store.is_dirty());
        assert_eq!(store.load_payload(&id).unwrap().unwrap(), original);
        store.flush().unwrap();
        assert!(!store.is_dirty());
        assert_eq!(store.load_payload(&id).unwrap().unwrap(), original);
        assert_eq!(
            fs::read(pin_dir(dir.path(), &id).join("canvas_session.bin")).unwrap(),
            original.canvas_session
        );
    }
    let restored = PinnedStore::open(dir.path(), PinOptions::default());
    assert_eq!(restored.record_ids(), vec![id.clone()]);
    assert_eq!(restored.load_payload(&id).unwrap().unwrap(), original);
    assert_eq!(read_manifest(dir.path())["format_version"], 2);
}

/// 缺失的可选 payload 不影响图片读取，清单在下次提交时摘掉该键（missingOptionalPayload…）。
#[test]
fn missing_optional_payload_does_not_hide_restorable_image() {
    let dir = TempDir::new("pin-missing");
    let id = new_uuid();
    {
        let mut store = PinnedStore::open(dir.path(), PinOptions::default());
        store
            .upsert(record(&id, "image_data"), Some(payload(8)))
            .unwrap();
        store.flush().unwrap();
        fs::remove_file(pin_dir(dir.path(), &id).join("recognition_results.bin")).unwrap();
        let loaded = store.load_payload(&id).unwrap().unwrap();
        assert!(loaded.recognition_results.is_empty() && !loaded.canvas_session.is_empty());
    }
    let mut reopened = PinnedStore::open(dir.path(), PinOptions::default());
    let loaded = reopened.load_payload(&id).unwrap().unwrap();
    assert!(loaded.image.is_some() && loaded.recognition_results.is_empty());
    let mut updated = record(&id, "image_data");
    updated.insert("closed".into(), json!(true));
    reopened.upsert(updated, None).unwrap();
    reopened.flush().unwrap();
    let manifest = read_manifest(dir.path());
    let payloads = manifest["records"][0]["payloads"].as_object().unwrap();
    assert!(
        !payloads.contains_key("recognition_results") && payloads.contains_key("canvas_session")
    );
}

/// 仅元数据更新不重写已提交 payload（metadataOnlyUpdatesDoNotRewriteCommittedPayloads）。
#[test]
fn metadata_only_updates_do_not_rewrite_committed_payloads() {
    let dir = TempDir::new("pin-meta");
    let id = new_uuid();
    let mut store = PinnedStore::open(dir.path(), PinOptions::default());
    let first = payload(9);
    store
        .upsert(record(&id, "image_data"), Some(first.clone()))
        .unwrap();
    store.flush().unwrap();
    assert_eq!(store.payload_writes(), 1);
    // 同一份 payload 反复传入，指纹一致，只更新清单。
    let mut meta = record(&id, "image_data");
    meta.insert("native_geometry".into(), json!({"x": 16}));
    store.upsert(meta.clone(), Some(first.clone())).unwrap();
    store.flush().unwrap();
    meta.insert("opacity_percent".into(), json!(87));
    store.upsert(meta, None).unwrap();
    store.flush().unwrap();
    assert_eq!(store.payload_writes(), 1);
    assert_eq!(store.record(&id).unwrap()["opacity_percent"], 87);
    assert_eq!(store.load_payload(&id).unwrap().unwrap(), first);
}

/// 真正变化的 payload 会重提交，且用新内容替换（changedPayloadsRecommitAndStayLazy）。
#[test]
fn changed_payloads_recommit() {
    let dir = TempDir::new("pin-changed");
    let id = new_uuid();
    let mut store = PinnedStore::open(dir.path(), PinOptions::default());
    store
        .upsert(record(&id, "image_data"), Some(payload(1)))
        .unwrap();
    store.flush().unwrap();
    let before = store.preview_source_revision(&id).unwrap();
    let second = payload(2);
    store
        .upsert(record(&id, "image_data"), Some(second.clone()))
        .unwrap();
    store.flush().unwrap();
    assert_eq!(store.payload_writes(), 2);
    assert_eq!(store.load_payload(&id).unwrap().unwrap(), second);
    assert!(store.preview_source_revision(&id).unwrap() > before);
    // 只改 canvas_session 不应推进预览源版本。
    let mut third = second.clone();
    third.canvas_session = b"{\"session\":99}".to_vec();
    let revision = store.preview_source_revision(&id).unwrap();
    store
        .upsert(record(&id, "image_data"), Some(third.clone()))
        .unwrap();
    store.flush().unwrap();
    assert_eq!(store.preview_source_revision(&id), Some(revision));
    assert_eq!(store.load_payload(&id).unwrap().unwrap(), third);
}

/// payload 变空后，对应文件被清理，清单摘掉引用。
#[test]
fn obsolete_payload_files_are_pruned() {
    let dir = TempDir::new("pin-prune");
    let id = new_uuid();
    let mut store = PinnedStore::open(dir.path(), PinOptions::default());
    store
        .upsert(record(&id, "image_data"), Some(payload(3)))
        .unwrap();
    store.flush().unwrap();
    let mut reduced = payload(3);
    reduced.canvas_session.clear();
    reduced.original_html.clear();
    store
        .upsert(record(&id, "image_data"), Some(reduced))
        .unwrap();
    store.flush().unwrap();
    let directory = pin_dir(dir.path(), &id);
    assert!(
        !directory.join("canvas_session.bin").exists() && !directory.join("original.html").exists()
    );
    assert!(directory.join("source.png").exists() && directory.join("original.txt").exists());
}

/// 删除记录后 payload 目录被清理（removedRecordsPruneTheirPayloads）。
#[test]
fn removed_records_prune_their_payloads() {
    let dir = TempDir::new("pin-remove");
    let id = new_uuid();
    let mut store = PinnedStore::open(dir.path(), PinOptions::default());
    store
        .upsert(record(&id, "image_data"), Some(payload(3)))
        .unwrap();
    store.flush().unwrap();
    assert!(store.remove(&id));
    store.flush().unwrap();
    assert!(store.load_payload(&id).unwrap().is_none());
    assert!(!pin_dir(dir.path(), &id).exists());
    assert!(
        read_manifest(dir.path())["records"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// 清单版本不是 2 或内容损坏：整体丢弃并留档（format_version 硬锁）。
#[test]
fn unsupported_or_malformed_manifest_is_discarded_and_preserved() {
    for content in [
        br#"{"format_version":1,"records":[]}"#.to_vec(),
        br#"{"format_version":3,"records":[]}"#.to_vec(),
        br#"{"records":[]}"#.to_vec(),
        b"broken".to_vec(),
    ] {
        let dir = TempDir::new("pin-version");
        let id = new_uuid();
        {
            let mut store = PinnedStore::open(dir.path(), PinOptions::default());
            store
                .upsert(record(&id, "image_data"), Some(payload(1)))
                .unwrap();
            store.flush().unwrap();
        }
        fs::write(manifest_path(dir.path()), &content).unwrap();
        let store = PinnedStore::open(dir.path(), PinOptions::default());
        assert!(store.record_ids().is_empty() && !store.last_error().is_empty());
        let backups: Vec<_> = fs::read_dir(dir.path().join("pinned_windows_v2"))
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains("index.json.corrupt.")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(backups[0].path()).unwrap(), content);
    }
}

/// 版本号写成 2.0 仍被接受（QJsonValue::toInt 语义）。
#[test]
fn manifest_version_accepts_integral_double() {
    let dir = TempDir::new("pin-double");
    let id = new_uuid();
    {
        let mut store = PinnedStore::open(dir.path(), PinOptions::default());
        store
            .upsert(record(&id, "image_data"), Some(payload(1)))
            .unwrap();
        store.flush().unwrap();
    }
    let mut manifest = read_manifest(dir.path());
    manifest["format_version"] = json!(2.0);
    fs::write(
        manifest_path(dir.path()),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    assert_eq!(
        PinnedStore::open(dir.path(), PinOptions::default()).record_ids(),
        vec![id]
    );
}

/// 清单里的路径穿越、非法 ID、未知分组、未知来源的记录被逐条丢弃，其余保留。
#[test]
fn hostile_manifest_records_are_skipped() {
    let dir = TempDir::new("pin-hostile");
    let (good, traversal, missing_image) = (new_uuid(), new_uuid(), new_uuid());
    {
        let mut store = PinnedStore::open(dir.path(), PinOptions::default());
        for id in [&good, &traversal, &missing_image] {
            store
                .upsert(record(id, "image_data"), Some(payload(1)))
                .unwrap();
        }
        store.flush().unwrap();
    }
    let mut manifest = read_manifest(dir.path());
    for entry in manifest["records"].as_array_mut().unwrap() {
        let id = entry["id"].as_str().unwrap().to_string();
        if id == traversal {
            entry["payloads"]["canvas_session"] = json!("../../outside.bin");
        } else if id == missing_image {
            fs::remove_file(pin_dir(dir.path(), &id).join("source.png")).unwrap();
        }
    }
    let records = manifest["records"].as_array_mut().unwrap();
    let mut bad_id = records[0].clone();
    bad_id["id"] = json!("../evil");
    let mut bad_group = records[0].clone();
    bad_group["id"] = json!(new_uuid());
    bad_group["group_id"] = json!("nope");
    let mut bad_kind = records[0].clone();
    bad_kind["id"] = json!(new_uuid());
    bad_kind["source_kind"] = json!("other");
    let mut wrong_directory = records[0].clone();
    wrong_directory["payloads"]["directory"] = json!("elsewhere");
    records.extend([bad_id, bad_group, bad_kind, wrong_directory]);
    fs::write(
        manifest_path(dir.path()),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let store = PinnedStore::open(dir.path(), PinOptions::default());
    // 穿越的 canvas_session 被判非法 -> 丢弃；图片缺失 -> 丢弃；good 保留。
    assert_eq!(store.record_ids(), vec![good]);
    assert_eq!(store.skipped_records(), 6);
}

/// 缺失文件的可选 payload 引用在载入时被摘掉。
#[test]
fn missing_optional_reference_is_dropped_on_load() {
    let dir = TempDir::new("pin-drop");
    let id = new_uuid();
    {
        let mut store = PinnedStore::open(dir.path(), PinOptions::default());
        store
            .upsert(record(&id, "image_data"), Some(payload(1)))
            .unwrap();
        store.flush().unwrap();
    }
    fs::remove_file(pin_dir(dir.path(), &id).join("original.txt")).unwrap();
    let store = PinnedStore::open(dir.path(), PinOptions::default());
    assert!(
        !store.record(&id).unwrap()["payloads"]
            .as_object()
            .unwrap()
            .contains_key("text")
    );
}

/// 体积上限：32 MiB 可写，超出被拒；磁盘上被撑大的文件读取失败。
#[test]
fn payload_size_limits() {
    let dir = TempDir::new("pin-size");
    let id = new_uuid();
    let mut store = PinnedStore::open(dir.path(), PinOptions::default());
    let mut over = payload(1);
    over.canvas_session = vec![b'x'; MAX_PAYLOAD_BYTES + 1];
    assert!(matches!(
        store.upsert(record(&id, "image_data"), Some(over)),
        Err(PinError::Invalid(_))
    ));
    let mut exact = payload(1);
    exact.canvas_session = vec![b'x'; MAX_PAYLOAD_BYTES];
    store
        .upsert(record(&id, "image_data"), Some(exact))
        .unwrap();
    store.flush().unwrap();
    assert_eq!(
        store
            .load_payload(&id)
            .unwrap()
            .unwrap()
            .canvas_session
            .len(),
        MAX_PAYLOAD_BYTES
    );
    fs::write(
        pin_dir(dir.path(), &id).join("canvas_session.bin"),
        vec![b'x'; MAX_PAYLOAD_BYTES + 1],
    )
    .unwrap();
    let reopened = PinnedStore::open(dir.path(), PinOptions::default());
    assert!(reopened.load_payload(&id).is_err());
}

/// 新记录必须带 payload；ID、分组、文件名不合法被拒。
#[test]
fn invalid_upserts_are_rejected() {
    let dir = TempDir::new("pin-invalid");
    let mut store = PinnedStore::open(dir.path(), PinOptions::default());
    let id = new_uuid();
    assert!(store.upsert(record(&id, "image_data"), None).is_err());
    assert!(
        store
            .upsert(record("../x", "image_data"), Some(payload(1)))
            .is_err()
    );
    let mut bad_group = record(&id, "image_data");
    bad_group.insert("group_id".into(), json!("missing"));
    assert!(store.upsert(bad_group, Some(payload(1))).is_err());
    let mut renamed = payload(1);
    renamed.image.as_mut().unwrap().file_name = "other.png".into();
    assert!(
        store
            .upsert(record(&id, "image_data"), Some(renamed))
            .is_err()
    );
    let mut unsafe_name = payload(1);
    unsafe_name.image.as_mut().unwrap().file_name = "..\\evil.png".into();
    assert!(
        store
            .upsert(record(&id, "clipboard_image_file"), Some(unsafe_name))
            .is_err()
    );
    let mut text_with_image = payload(1);
    text_with_image.original_text = "x".into();
    assert!(
        store
            .upsert(record(&id, "clipboard_text"), Some(text_with_image))
            .is_err()
    );
    assert!(store.record_ids().is_empty());
    // 文本贴图与沿用原文件名的图片文件贴图可以正常往返。
    let text_id = new_uuid();
    let text_only = PinPayload {
        original_text: "hello".into(),
        ..PinPayload::default()
    };
    store
        .upsert(record(&text_id, "clipboard_text"), Some(text_only.clone()))
        .unwrap();
    let file_id = new_uuid();
    let mut file_payload = payload(4);
    file_payload.image.as_mut().unwrap().file_name = "photo.jpg".into();
    store
        .upsert(
            record(&file_id, "clipboard_image_file"),
            Some(file_payload.clone()),
        )
        .unwrap();
    store.flush().unwrap();
    let reopened = PinnedStore::open(dir.path(), PinOptions::default());
    assert_eq!(reopened.load_payload(&text_id).unwrap().unwrap(), text_only);
    assert_eq!(
        reopened.load_payload(&file_id).unwrap().unwrap(),
        file_payload
    );
}

/// 分组解析规则：默认分组在前，重名/超长/重复 ID/非法 ID 被忽略，数量封顶 128，激活分组回落。
#[test]
fn groups_are_sanitized() {
    let dir = TempDir::new("pin-groups");
    let mut store = PinnedStore::open(dir.path(), PinOptions::default());
    let keep = new_uuid();
    let mut groups = vec![
        PinGroup {
            id: "default".into(),
            name: "Again".into(),
            built_in: false,
        },
        PinGroup {
            id: keep.clone(),
            name: " Work ".into(),
            built_in: false,
        },
        PinGroup {
            id: new_uuid(),
            name: "work".into(),
            built_in: false,
        },
        PinGroup {
            id: new_uuid(),
            name: "x".repeat(17),
            built_in: false,
        },
        PinGroup {
            id: "not-uuid".into(),
            name: "Bad".into(),
            built_in: false,
        },
        PinGroup {
            id: new_uuid(),
            name: "  ".into(),
            built_in: false,
        },
    ];
    store.set_groups(&groups, &keep);
    let names: Vec<String> = store.groups().into_iter().map(|g| g.name).collect();
    assert_eq!(names, vec!["Default", "Work"]);
    assert_eq!(store.active_group_id(), keep);
    groups.clear();
    for i in 0..200 {
        groups.push(PinGroup {
            id: new_uuid(),
            name: format!("g{i}"),
            built_in: false,
        });
    }
    store.set_groups(&groups, "missing");
    assert_eq!(store.groups().len(), 128);
    assert_eq!(store.active_group_id(), "default");
    store.flush().unwrap();
    let reopened = PinnedStore::open(dir.path(), PinOptions::default());
    assert_eq!(reopened.groups().len(), 128);
    assert!(!reopened.groups()[0].name.is_empty() && reopened.groups()[0].built_in);
}

/// 只读仓储拒绝写入；upstream 目录一律拒绝且不产生 I/O。
#[test]
fn read_only_and_upstream_are_refused() {
    let dir = TempDir::new("pin-readonly");
    let options = PinOptions {
        write_available: false,
        ..PinOptions::default()
    };
    let mut store = PinnedStore::open(dir.path(), options);
    assert!(matches!(store.flush(), Err(PinError::Unavailable(_))));
    assert!(
        store
            .upsert(record(&new_uuid(), "image_data"), Some(payload(1)))
            .is_err()
    );
    let upstream = Path::new(r"Z:\definitely-missing\SnowShot\snow_shot");
    let mut refused = PinnedStore::open(upstream, PinOptions::default());
    assert!(!refused.last_error().is_empty() && refused.flush().is_err());
    assert!(!upstream.exists());
}

/// 以给定崩溃点构造选项。
fn crash_at(point: PinCrashPoint) -> PinOptions {
    PinOptions {
        fault_hook: Some(Rc::new(move |p| p == point)),
        ..PinOptions::default()
    }
}

/// 崩溃一致性：payload 写完、清单未提交 -> 重开看到旧清单；孤儿目录仅在显式清扫后消失。
#[test]
fn crash_after_payloads_written() {
    let dir = TempDir::new("pin-crash1");
    let (old, new) = (new_uuid(), new_uuid());
    {
        let mut store = PinnedStore::open(dir.path(), PinOptions::default());
        store
            .upsert(record(&old, "image_data"), Some(payload(1)))
            .unwrap();
        store.flush().unwrap();
    }
    {
        let mut store =
            PinnedStore::open(dir.path(), crash_at(PinCrashPoint::AfterPayloadsWritten));
        store
            .upsert(record(&new, "image_data"), Some(payload(2)))
            .unwrap();
        assert_eq!(
            store.flush(),
            Err(PinError::Crashed(PinCrashPoint::AfterPayloadsWritten))
        );
    }
    let mut store = PinnedStore::open(dir.path(), PinOptions::default());
    assert_eq!(store.record_ids(), vec![old.clone()]);
    assert_eq!(store.load_payload(&old).unwrap().unwrap(), payload(1));
    assert!(pin_dir(dir.path(), &new).exists());
    assert_eq!(store.sweep_orphans().unwrap(), 1);
    assert!(!pin_dir(dir.path(), &new).exists() && pin_dir(dir.path(), &old).exists());
}

/// 崩溃一致性：清单已提交（记录已删）、目录未清理 -> 重开记录不在，清扫后无孤儿；未变更的记录不受影响。
#[test]
fn crash_after_manifest_committed() {
    let dir = TempDir::new("pin-crash2");
    let (keep, gone) = (new_uuid(), new_uuid());
    {
        let mut store = PinnedStore::open(dir.path(), PinOptions::default());
        store
            .upsert(record(&keep, "image_data"), Some(payload(1)))
            .unwrap();
        store
            .upsert(record(&gone, "image_data"), Some(payload(2)))
            .unwrap();
        store.flush().unwrap();
    }
    {
        let mut store =
            PinnedStore::open(dir.path(), crash_at(PinCrashPoint::AfterManifestCommitted));
        assert!(store.remove(&gone));
        let mut changed = payload(1);
        changed.canvas_session = b"{\"session\":7}".to_vec();
        store
            .upsert(record(&keep, "image_data"), Some(changed))
            .unwrap();
        assert_eq!(
            store.flush(),
            Err(PinError::Crashed(PinCrashPoint::AfterManifestCommitted))
        );
    }
    let mut store = PinnedStore::open(dir.path(), PinOptions::default());
    assert_eq!(store.record_ids(), vec![keep.clone()]);
    assert_eq!(
        store.load_payload(&keep).unwrap().unwrap().canvas_session,
        b"{\"session\":7}"
    );
    assert!(pin_dir(dir.path(), &gone).exists());
    assert_eq!(store.sweep_orphans().unwrap(), 1);
    assert!(!pin_dir(dir.path(), &gone).exists());
    assert_eq!(
        store.load_payload(&keep).unwrap().unwrap().canvas_session,
        b"{\"session\":7}"
    );
}

/// 崩溃一致性：提交失败（清单被目录占位）后内存仍脏，修复后重试成功。
#[test]
fn failed_manifest_write_is_retried() {
    let dir = TempDir::new("pin-retry");
    let id = new_uuid();
    let mut store = PinnedStore::open(dir.path(), PinOptions::default());
    store
        .upsert(record(&id, "image_data"), Some(payload(1)))
        .unwrap();
    fs::create_dir_all(manifest_path(dir.path())).unwrap();
    assert!(store.flush().is_err() && store.is_dirty() && !store.last_error().is_empty());
    fs::remove_dir(manifest_path(dir.path())).unwrap();
    store.flush().unwrap();
    assert!(!store.is_dirty() && store.last_error().is_empty());
    let reopened = PinnedStore::open(dir.path(), PinOptions::default());
    assert_eq!(reopened.load_payload(&id).unwrap().unwrap(), payload(1));
}
