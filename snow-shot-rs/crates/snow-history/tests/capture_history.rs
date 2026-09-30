//! 截图历史仓储测试：移植自 C++ `capture_history_repository_tests.cpp`，外加崩溃一致性。

mod common;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use common::*;
use serde_json::json;
use snow_history::capture_history::{
    CaptureHistoryPolicy, CaptureHistoryRepository, CrashPoint, DraftImage, HistoryError, Options,
};
use snow_history::timeutil::{MILLIS_PER_DAY, now_utc_ms, parse_iso_utc_ms};

/// 固定时钟：2026-08-05T12:30:00.000Z。
const NOW: i64 = 1_785_933_000_000;
/// 一 MiB。
const MIB: usize = 1024 * 1024;

/// 以固定时钟构造选项。
fn options_at(now: i64) -> Options {
    Options {
        clock: Arc::new(move || now),
        ..Options::default()
    }
}

/// 以给定崩溃点构造选项。
fn crash_at(point: CrashPoint) -> Options {
    Options {
        clock: Arc::new(|| NOW),
        fault_hook: Some(Arc::new(move |p| p == point)),
        ..Options::default()
    }
}

/// 磁盘上某记录目录内文件大小。
fn file_len(root: &Path, id: &str, name: &str) -> i64 {
    fs::metadata(records_dir(root).join(id).join(name))
        .unwrap()
        .len() as i64
}

/// 发布后 manifest、字节记账与重开恢复（publicationAndRecovery）。
#[test]
fn publication_and_recovery() {
    let dir = TempDir::new("publish");
    let mut draft = draft_at(NOW);
    draft.source = "pinned_to_screen".into();
    draft.result = Some(DraftImage {
        width: 20,
        height: 12,
        png: vec![9; 55],
    });
    let published = {
        let mut repo = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
        let record = repo.publish(draft).unwrap();
        let manifest = read_index(dir.path());
        assert_eq!(manifest["format_version"], 2);
        assert_eq!(manifest["records"][0]["source"], "pinned_to_screen");
        let physical = file_len(dir.path(), &record.id, "canvas_history.json")
            + file_len(dir.path(), &record.id, "display_0.png")
            + file_len(dir.path(), &record.id, "capture_result.png");
        assert_eq!(record.total_record_size, physical);
        assert_eq!(manifest["records"][0]["total_record_size"], physical);
        let usage = repo.usage();
        let index_bytes = fs::metadata(index_path(dir.path())).unwrap().len() as i64;
        assert_eq!((usage.entry_count, usage.record_bytes), (1, physical));
        assert_eq!(usage.index_bytes, index_bytes);
        assert_eq!(usage.total_bytes, physical + index_bytes);
        let canvas = repo.load_canvas(&record).unwrap();
        assert_eq!(canvas.len() as i64, record.canvas_byte_size);
        assert_eq!(
            repo.read_image_file(&record, "capture_result.png")
                .unwrap()
                .len(),
            55
        );
        assert_eq!(
            repo.read_image_file(&record, "display_0.png")
                .unwrap()
                .len(),
            100
        );
        record
    };
    let recovered = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
    assert_eq!(recovered.records(), vec![published]);
}

/// 启动不检查 payload 也不扫描遗留；显式 clear 清掉受管树内一切（trustedStartupAndExplicitClear）。
#[test]
fn trusted_startup_and_explicit_clear() {
    let dir = TempDir::new("trusted");
    let record = {
        let mut repo = CaptureHistoryRepository::open(dir.path(), Options::default());
        repo.publish(draft_at(now_utc_ms())).unwrap()
    };
    fs::write(
        records_dir(dir.path())
            .join(&record.id)
            .join("canvas_history.json"),
        b"not-json",
    )
    .unwrap();
    let leftover = records_dir(dir.path()).join(".tmp-abandoned");
    fs::create_dir_all(&leftover).unwrap();
    fs::write(leftover.join("partial"), b"x").unwrap();

    let mut repo = CaptureHistoryRepository::open(dir.path(), Options::default());
    assert!(repo.records().len() == 1 && leftover.exists());
    repo.clear().unwrap();
    let usage = repo.usage();
    assert_eq!(usage.entry_count, 0);
    assert_eq!(usage.total_bytes, usage.index_bytes);
    assert!(!leftover.exists());
}

/// 年龄边界、禁用不淘汰、重新启用后按容量淘汰（policyBoundariesAndDisabledPreservation）。
#[test]
fn policy_boundaries_and_disabled_preservation() {
    let dir = TempDir::new("policy");
    let mut options = options_at(NOW);
    options.policy.retention_days = 365;
    let mut repo = CaptureHistoryRepository::open(dir.path(), options.clone());
    let exact = NOW - 7 * MILLIS_PER_DAY;
    repo.publish(draft_at(exact)).unwrap();
    repo.publish(draft_at(exact - 1)).unwrap();
    let mut policy = options.policy.clone();
    policy.retention_days = 7;
    repo.update_policy(policy.clone()).unwrap();
    let kept = repo.records();
    assert_eq!(kept.len(), 1);
    assert_eq!(parse_iso_utc_ms(&kept[0].created_utc), Some(exact));

    repo.publish(draft_at(NOW - 2000)).unwrap();
    repo.publish(draft_at(NOW - 1000)).unwrap();
    policy.enabled = false;
    policy.max_entries = 1;
    repo.update_policy(policy.clone()).unwrap();
    assert_eq!(repo.records().len(), 3);
    assert!(repo.publish(draft_at(NOW)).is_err());
    assert_eq!(repo.records().len(), 3);
    policy.enabled = true;
    repo.update_policy(policy).unwrap();
    assert_eq!(repo.records().len(), 3);
    let added = repo.publish(draft_at(NOW)).unwrap();
    let records = repo.records();
    assert!(records.len() == 1 && records[0].id == added.id);
}

/// manifest 中的路径穿越文件名使整份索引作废（traversalManifestIsRejected）。
#[test]
fn traversal_manifest_is_rejected() {
    let dir = TempDir::new("traversal");
    {
        let mut repo = CaptureHistoryRepository::open(dir.path(), Options::default());
        repo.publish(draft_at(NOW)).unwrap();
    }
    let mut index = read_index(dir.path());
    index["records"][0]["displays"][0]["image_file"] = json!("../outside.png");
    write_index(dir.path(), &index);
    let repo = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
    assert!(repo.records().is_empty());
    assert!(!repo.last_error().is_empty());
}

/// 索引损坏时保留全部文件、拒绝发布，clear 后恢复（indexFailurePreservesFilesUntilClear）。
#[test]
fn index_failure_preserves_files_until_clear() {
    let dir = TempDir::new("brokenindex");
    {
        let mut repo = CaptureHistoryRepository::open(dir.path(), Options::default());
        repo.publish(draft_at(now_utc_ms())).unwrap();
    }
    let names = record_dir_names(dir.path());
    fs::write(index_path(dir.path()), b"broken index").unwrap();
    let mut repo = CaptureHistoryRepository::open(dir.path(), Options::default());
    assert!(!repo.last_error().is_empty() && repo.records().is_empty());
    assert_eq!(record_dir_names(dir.path()), names);
    assert!(repo.publish(draft_at(NOW)).is_err());
    assert_eq!(fs::read(index_path(dir.path())).unwrap(), b"broken index");
    repo.clear().unwrap();
    assert!(repo.publish(draft_at(now_utc_ms())).is_ok());
}

/// 索引提交失败不损伤已确认的历史，也不推进 revision（failedCommitPreservesPublishedHistory）。
#[test]
fn failed_commit_preserves_published_history() {
    let dir = TempDir::new("failedcommit");
    let mut repo = CaptureHistoryRepository::open(dir.path(), Options::default());
    let now = now_utc_ms();
    let first = repo.publish(draft_at(now)).unwrap();
    let mut policy = repo.policy();
    policy.max_entries = 1;
    repo.update_policy(policy).unwrap();
    let index = index_path(dir.path());
    let saved = index.with_extension("json.saved");
    fs::rename(&index, &saved).unwrap();
    fs::create_dir(&index).unwrap();
    let revision = repo.revision();
    assert!(repo.publish(draft_at(now + 1)).is_err());
    assert_eq!(repo.records(), vec![first.clone()]);
    assert!(repo.load_canvas(&first).is_some());
    assert_eq!(repo.revision(), revision);
    fs::remove_dir(&index).unwrap();
    fs::rename(&saved, &index).unwrap();
    drop(repo);
    let reopened = CaptureHistoryRepository::open(dir.path(), Options::default());
    assert_eq!(reopened.records(), vec![first]);
    // 失败的发布不应留下未入索引的记录目录。
    assert_eq!(record_dir_names(dir.path()).len(), 1);
}

/// 启动补删 pending，但不扫描无关目录（pendingDeletionResumesWithoutScanningOrphans）。
#[test]
fn pending_deletion_resumes_without_scanning_orphans() {
    let dir = TempDir::new("pending");
    let record = {
        let mut repo = CaptureHistoryRepository::open(dir.path(), Options::default());
        repo.publish(draft_at(NOW)).unwrap()
    };
    let mut index = read_index(dir.path());
    index["records"] = json!([]);
    index["pending_deletions"] = json!([{"id": record.id, "bytes": record.total_record_size}]);
    write_index(dir.path(), &index);
    let orphan = records_dir(dir.path()).join(".tmp-orphan");
    fs::create_dir_all(&orphan).unwrap();
    let mut repo = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
    assert!(repo.needs_maintenance());
    repo.maintenance().unwrap();
    assert!(!records_dir(dir.path()).join(&record.id).exists() && orphan.exists());
    assert_eq!(repo.usage().pending_deletion_bytes, 0);
    assert!(
        read_index(dir.path())["pending_deletions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// 启动只按年龄淘汰、不按容量；只读仓储不改动历史（startupExpiresAgeButDoesNotEnforceCapacity）。
#[test]
fn startup_expires_age_but_does_not_enforce_capacity() {
    let dir = TempDir::new("startup");
    let mut options = options_at(NOW);
    options.policy.retention_days = 365;
    {
        let mut writer = CaptureHistoryRepository::open(dir.path(), options.clone());
        for date in [NOW - 8 * MILLIS_PER_DAY, NOW - 7 * MILLIS_PER_DAY, NOW] {
            writer.publish(draft_at(date)).unwrap();
        }
    }
    options.policy.retention_days = 7;
    options.policy.max_entries = 1;
    let mut repo = CaptureHistoryRepository::open(dir.path(), options.clone());
    assert!(repo.needs_maintenance());
    repo.maintenance().unwrap();
    let records = repo.records();
    assert_eq!(records.len(), 2);
    assert_eq!(
        parse_iso_utc_ms(&records[1].created_utc),
        Some(NOW - 7 * MILLIS_PER_DAY)
    );
    drop(repo);
    options.write_available = false;
    options.policy.retention_days = 1;
    let mut read_only = CaptureHistoryRepository::open(dir.path(), options);
    assert_eq!(read_only.records().len(), 2);
    assert!(read_only.clear().is_err());
    assert!(read_only.maintenance().is_err());
}

/// 批量删除只提交一次索引，revision 只 +1（batchRemovalCommitsAndNotifiesOnce）。
#[test]
fn batch_removal_commits_once() {
    let dir = TempDir::new("batch");
    let mut repo = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
    let ids: Vec<String> = (0..3)
        .map(|i| repo.publish(draft_at(NOW - i)).unwrap().id)
        .collect();
    let revision = repo.revision();
    repo.remove_many(&[ids[0].clone(), ids[1].clone(), "missing".into()])
        .unwrap();
    assert_eq!(repo.revision(), revision + 1);
    assert_eq!(repo.records().len(), 1);
    assert_eq!(record_dir_names(dir.path()), vec![ids[2].clone()]);
    repo.remove_many(&["missing".into()]).unwrap();
    assert_eq!(repo.revision(), revision + 1);
    assert_eq!(repo.usage().pending_deletion_bytes, 0);
}

/// 永久保留绕过年龄、条数、体积限制（permanentHistoryBypassesLimitsAndAllowsManualDeletion）。
#[test]
fn permanent_history_bypasses_limits_and_allows_manual_deletion() {
    let dir = TempDir::new("permanent");
    let mut options = options_at(NOW);
    options.policy = CaptureHistoryPolicy {
        retention_days: 1,
        max_entries: 1,
        max_disk_mib: CaptureHistoryPolicy::MIN_DISK_MIB,
        ..CaptureHistoryPolicy::default()
    };
    assert!(!options.policy.keep_permanently);
    let quota = options.policy.max_disk_mib as usize * MIB;
    let mut repo = CaptureHistoryRepository::open(dir.path(), options.clone());
    let mut oversized = draft_at(NOW);
    oversized.result = Some(DraftImage {
        width: 32,
        height: 24,
        png: vec![0; quota + 1],
    });
    assert!(repo.publish(oversized.clone()).is_err());
    options.policy.keep_permanently = true;
    repo.update_policy(options.policy.clone()).unwrap();
    repo.publish(oversized.clone()).unwrap();
    repo.publish(draft_at(NOW - 400 * MILLIS_PER_DAY)).unwrap();
    assert_eq!(repo.records().len(), 2);
    assert!(repo.usage().record_bytes > quota as i64);
    drop(repo);
    let mut repo = CaptureHistoryRepository::open(dir.path(), options.clone());
    repo.maintenance().unwrap();
    assert_eq!(repo.records().len(), 2);
    let mut bounded = options.policy.clone();
    bounded.keep_permanently = false;
    repo.update_policy(bounded).unwrap();
    assert_eq!(repo.records().len(), 1);
    let latest = repo.publish(draft_at(NOW + 1000)).unwrap();
    assert!(repo.records().len() == 1 && repo.records()[0].id == latest.id);
    repo.update_policy(options.policy).unwrap();
    repo.clear().unwrap();
    assert!(repo.records().is_empty());
}

/// revision 语义逐条对照（revisionCheckedMutations）。
#[test]
fn revision_checked_mutations() {
    let dir = TempDir::new("revision");
    let mut repo = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
    let initial = repo.revision();
    let first = draft_at(NOW);
    repo.publish(first.clone()).unwrap();
    let published = repo.revision();
    assert!(published > initial && repo.records().len() == 1);
    repo.remove_if_revision(&["missing".into()], published, false)
        .unwrap();
    assert_eq!(repo.revision(), published);
    assert_eq!(
        repo.remove_if_revision(std::slice::from_ref(&first.id), initial, false),
        Err(HistoryError::StaleRevision)
    );
    assert_eq!(repo.records().len(), 1);
    let second = draft_at(NOW + 1000);
    repo.publish(second.clone()).unwrap();
    assert_eq!(
        repo.remove_if_revision(&[], published, true),
        Err(HistoryError::StaleRevision)
    );
    assert_eq!(repo.records().len(), 2);
    let current = repo.revision();
    repo.remove_if_revision(std::slice::from_ref(&first.id), current, false)
        .unwrap();
    let records = repo.records();
    assert!(records.len() == 1 && records[0].id == second.id);
    let removed = repo.revision();
    assert_eq!(removed, published + 2);
    let mut policy = repo.policy();
    policy.max_entries = 1;
    repo.update_policy(policy).unwrap();
    assert_eq!(repo.revision(), removed);
    let replacement = draft_at(NOW + 2000);
    repo.publish(replacement.clone()).unwrap();
    assert_eq!(repo.revision(), removed + 1);
    assert!(repo.records().len() == 1 && repo.records()[0].id == replacement.id);
    let replaced = repo.revision();
    repo.remove_if_revision(&[], replaced, true).unwrap();
    assert_eq!(repo.revision(), replaced + 1);
    repo.clear().unwrap();
    assert_eq!(repo.revision(), replaced + 1);
}

/// 旧版本 1 的索引仍可读取，且重写后升到 2。
#[test]
fn legacy_v1_index_is_readable_and_upgraded() {
    let dir = TempDir::new("legacy");
    {
        let mut repo = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
        repo.publish(draft_at(NOW)).unwrap();
    }
    let mut index = read_index(dir.path());
    index["format_version"] = json!(1);
    write_index(dir.path(), &index);
    let mut repo = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
    assert_eq!(repo.records().len(), 1);
    repo.publish(draft_at(NOW + 1)).unwrap();
    assert_eq!(read_index(dir.path())["format_version"], 2);
}

/// 版本号未知时整份索引作废。
#[test]
fn unknown_index_version_is_rejected() {
    let dir = TempDir::new("version");
    {
        let mut repo = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
        repo.publish(draft_at(NOW)).unwrap();
    }
    let mut index = read_index(dir.path());
    index["format_version"] = json!(3);
    write_index(dir.path(), &index);
    let repo = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
    assert!(repo.records().is_empty() && !repo.last_error().is_empty());
}

/// 草稿校验：非法 ID、超大 canvas、非 JSON canvas、图片型缺结果图、未知来源均被拒绝。
#[test]
fn invalid_drafts_are_rejected() {
    let dir = TempDir::new("invalid");
    let mut repo = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
    let mut bad_id = draft_at(NOW);
    bad_id.id = "not-a-uuid".into();
    let mut bad_canvas = draft_at(NOW);
    bad_canvas.canvas_history = b"not-json".to_vec();
    let mut scalar_canvas = draft_at(NOW);
    scalar_canvas.canvas_history = b"42".to_vec();
    let mut huge_canvas = draft_at(NOW);
    huge_canvas.canvas_history = vec![b' '; 16 * MIB + 1];
    huge_canvas.canvas_history[0] = b'{';
    let mut image_kind = draft_at(NOW);
    image_kind.content_image = true;
    let mut bad_source = draft_at(NOW);
    bad_source.source = "elsewhere".into();
    let mut empty_png = draft_at(NOW);
    empty_png.displays[0].image.png.clear();
    for draft in [
        bad_id,
        bad_canvas,
        scalar_canvas,
        huge_canvas,
        image_kind,
        bad_source,
        empty_png,
    ] {
        assert!(repo.publish(draft).is_err());
    }
    let duplicate = draft_at(NOW);
    repo.publish(duplicate.clone()).unwrap();
    assert!(repo.publish(duplicate).is_err());
    assert_eq!(repo.records().len(), 1);
}

/// 读盘校验：canvas 被改写后长度不符，读取失败且记录被移除。
#[test]
fn tampered_canvas_is_removed_on_read() {
    let dir = TempDir::new("tamper");
    let mut repo = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
    let record = repo.publish(draft_at(NOW)).unwrap();
    fs::write(
        records_dir(dir.path())
            .join(&record.id)
            .join("canvas_history.json"),
        b"{}",
    )
    .unwrap();
    assert!(repo.load_canvas(&record).is_none());
    assert!(repo.records().is_empty());
    assert!(!records_dir(dir.path()).join(&record.id).exists());
}

/// upstream 数据目录一律拒绝，且不产生任何 I/O。
#[test]
fn upstream_directory_is_refused() {
    let path = Path::new(r"Z:\definitely-missing\SnowShot\snow_shot");
    let mut repo = CaptureHistoryRepository::open(path, Options::default());
    assert!(!repo.last_error().is_empty());
    assert!(repo.publish(draft_at(NOW)).is_err() && repo.clear().is_err());
    assert!(!path.exists());
}

/// 真实脱敏样本：可打开、可读 canvas 与图片。
#[test]
fn real_sample_loads() {
    let dir = TempDir::new("sample");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample");
    copy_tree(&source, &dir.path().join("capture_history"));
    let mut repo = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
    assert!(repo.last_error().is_empty(), "{}", repo.last_error());
    let records = repo.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(
        repo.load_canvas(record).unwrap().len() as i64,
        record.canvas_byte_size
    );
    for display in &record.displays {
        let bytes = repo.read_image_file(record, &display.image_file).unwrap();
        assert_eq!(bytes.len() as i64, display.encoded_bytes);
    }
    assert!(repo.read_image_file(record, "../index.json").is_none());
}

/// 递归复制目录。
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// 重开、补维护，并检查一致性：索引可读、每条现存记录 canvas 可读且字节数一致、pending 已空。
fn reopen_and_check(dir: &Path) -> CaptureHistoryRepository {
    let mut repo = CaptureHistoryRepository::open(dir, options_at(NOW));
    assert!(repo.last_error().is_empty(), "{}", repo.last_error());
    repo.maintenance().unwrap();
    assert_eq!(repo.usage().pending_deletion_bytes, 0);
    for record in repo.records() {
        let canvas = repo.load_canvas(&record).unwrap();
        assert_eq!(canvas.len() as i64, record.canvas_byte_size);
    }
    repo
}

/// 崩溃一致性：publish 的三个中间点。
#[test]
fn crash_during_publish() {
    for (point, survives) in [
        (CrashPoint::AfterTempFilesWritten, false),
        (CrashPoint::AfterRecordRenamed, false),
        (CrashPoint::AfterIndexCommittedWithPending, true),
    ] {
        let dir = TempDir::new("crashpub");
        let draft = draft_at(NOW);
        let result = {
            let mut repo = CaptureHistoryRepository::open(dir.path(), crash_at(point));
            repo.publish(draft.clone())
        };
        assert_eq!(result.unwrap_err(), HistoryError::Crashed(point));
        let mut repo = reopen_and_check(dir.path());
        assert_eq!(repo.records().len(), usize::from(survives), "{point:?}");
        if survives {
            assert_eq!(record_dir_names(dir.path()), vec![draft.id.clone()]);
        } else {
            // C++ 既有行为：启动不扫描孤儿，遗留目录只由显式 clear 清除。
            assert_eq!(record_dir_names(dir.path()).len(), 1, "{point:?}");
            repo.clear().unwrap();
            assert!(record_dir_names(dir.path()).is_empty());
        }
    }
}

/// 崩溃一致性：publish 触发容量淘汰时，在 pending 提交后与目录删除后崩溃。
#[test]
fn crash_during_capacity_eviction() {
    for point in [
        CrashPoint::AfterIndexCommittedWithPending,
        CrashPoint::AfterPayloadRemoved,
    ] {
        let dir = TempDir::new("crashevict");
        let old = draft_at(NOW - 1000);
        {
            let mut options = options_at(NOW);
            options.policy.max_entries = 1;
            let mut repo = CaptureHistoryRepository::open(dir.path(), options);
            repo.publish(old.clone()).unwrap();
        }
        let newer = draft_at(NOW);
        {
            let mut options = crash_at(point);
            options.policy.max_entries = 1;
            let mut repo = CaptureHistoryRepository::open(dir.path(), options);
            assert_eq!(
                repo.publish(newer.clone()).unwrap_err(),
                HistoryError::Crashed(point)
            );
        }
        let repo = reopen_and_check(dir.path());
        let ids: Vec<String> = repo.records().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![newer.id.clone()], "{point:?}");
        assert_eq!(
            record_dir_names(dir.path()),
            vec![newer.id.clone()],
            "{point:?}"
        );
    }
}

/// 崩溃一致性：remove_many 在目录删除后、pending 摘除提交前崩溃，重开后补删完成。
#[test]
fn crash_during_remove_many() {
    let dir = TempDir::new("crashremove");
    let (keep, drop_id) = {
        let mut repo = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
        let keep = repo.publish(draft_at(NOW)).unwrap().id;
        (keep, repo.publish(draft_at(NOW - 1000)).unwrap().id)
    };
    {
        let mut repo =
            CaptureHistoryRepository::open(dir.path(), crash_at(CrashPoint::AfterPayloadRemoved));
        let outcome = repo.remove_many(std::slice::from_ref(&drop_id));
        assert_eq!(
            outcome.unwrap_err(),
            HistoryError::Crashed(CrashPoint::AfterPayloadRemoved)
        );
    }
    // 此刻索引里 pending 仍含该 id、目录已不存在。
    assert_eq!(
        read_index(dir.path())["pending_deletions"][0]["id"],
        drop_id
    );
    let repo = reopen_and_check(dir.path());
    let ids: Vec<String> = repo.records().into_iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![keep.clone()]);
    assert_eq!(record_dir_names(dir.path()), vec![keep]);
}

/// 手工构造“索引已摘除、目录仍在”的中间态（等价于 remove_many 在 pending 提交后崩溃），重开补删。
#[test]
fn crash_after_pending_commit_before_delete() {
    let dir = TempDir::new("crashpending");
    let (keep, gone) = {
        let mut repo = CaptureHistoryRepository::open(dir.path(), options_at(NOW));
        (
            repo.publish(draft_at(NOW)).unwrap(),
            repo.publish(draft_at(NOW - 1000)).unwrap(),
        )
    };
    let mut index = read_index(dir.path());
    index["records"] = json!([index["records"][0].clone()]);
    index["pending_deletions"] = json!([{"id": gone.id, "bytes": gone.total_record_size}]);
    write_index(dir.path(), &index);
    assert!(records_dir(dir.path()).join(&gone.id).exists());
    let repo = reopen_and_check(dir.path());
    assert_eq!(repo.records(), vec![keep.clone()]);
    assert_eq!(record_dir_names(dir.path()), vec![keep.id]);
}

/// 末尾带点的 upstream 组件同样被拒绝，且不创建目录。
#[test]
fn trailing_dot_upstream_is_refused() {
    let dir = TempDir::new("hist-dot");
    let path = dir.path().join("SnowShot.").join("snow_shot");
    let repo = CaptureHistoryRepository::open(&path, Options::default());
    assert!(!repo.last_error().is_empty());
    assert!(!dir.path().join("SnowShot.").exists());
}

/// 仓储与选项可跨线程移动。
#[test]
fn repository_and_options_are_send() {
    fn assert_send<T: Send>() {}
    assert_send::<CaptureHistoryRepository>();
    assert_send::<Options>();
}
