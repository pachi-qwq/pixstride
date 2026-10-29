//! 只在新建的隔离缓存根中注入损坏；断言实际文件状态与受管理载荷用量。

use super::*;
use crate::large::stream::tests::{generate, root};

fn owned(directory: &Path, index: &[u8], pixels: Option<u64>) -> Result<()> {
    fs::create_dir(directory)?;
    fs::write(directory.join("owner"), MAGIC)?;
    fs::write(directory.join("index.bin"), index)?;
    if let Some(bytes) = pixels {
        File::create(directory.join("pixels.bin"))?.set_len(bytes)?;
    }
    Ok(())
}

fn check_tiles(cache: &DiskCache) -> Result<()> {
    assert_eq!(
        fs::metadata(cache.directory.join("pixels.bin"))?.len(),
        cache.plan.bytes
    );
    for id in 0..cache.plan.tiles.len() {
        cache.read_tile(id)?;
    }
    Ok(())
}

fn cached_payload(directory: &Path) -> Result<u64> {
    Ok(fs::metadata(directory.join("index.bin"))?.len()
        + fs::metadata(directory.join("pixels.bin"))?.len())
}

/// 逐项读回磁盘上的 index+pixels，不依赖归属判断，供精确计费断言使用。
fn actual_cache_payload(root: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("cache-") {
            total = total
                .checked_add(cached_payload(&entry.path())?)
                .context("测试载荷统计溢出")?;
        }
    }
    Ok(total)
}

/// 目录不能用 File::open 打开；只请求 FILE_WRITE_ATTRIBUTES 并带 BACKUP_SEMANTICS。
fn set_directory_modified(directory: &Path, modified: std::time::SystemTime) -> Result<()> {
    let handle = OpenOptions::new()
        .access_mode(0x100)
        .custom_flags(BACKUP_SEMANTICS)
        .open(directory)?;
    handle.set_modified(modified)?;
    assert_eq!(
        fs::metadata(directory)?.modified()?,
        modified,
        "目录 mtime 必须真实写回，不能依赖枚举或 sleep 的偶然顺序"
    );
    Ok(())
}

#[test]
fn a1_owner_initialization_failure_rolls_back_only_new_partial() -> Result<()> {
    let test = root("a1-owner-write");
    for written in [0, 4] {
        let partial = test.join(format!(".build-short-{written}"));
        let error = PartialBuild::create(&partial, |owner| {
            owner.write_all(&MAGIC[..written])?;
            Err(io::Error::from_raw_os_error(112))
        })
        .err()
        .context("owner 初始化失败应保留原始 I/O 错误")?;
        assert_eq!(
            error
                .downcast_ref::<io::Error>()
                .and_then(|e| e.raw_os_error()),
            Some(112)
        );
        assert!(!partial.exists(), "零字节或短 owner 不能留下永久孤儿");
    }
    let extra = test.join(".build-unknown-member");
    assert!(
        PartialBuild::create(&extra, |owner| {
            owner.write_all(&MAGIC[..4])?;
            fs::write(extra.join("keep"), b"preserve")?;
            Err(io::Error::from_raw_os_error(112))
        })
        .is_err()
    );
    assert_eq!(fs::read(extra.join("keep"))?, b"preserve");
    assert!(!extra.join("owner").exists(), "只删除本次新建的 owner 句柄");
    fs::remove_dir_all(test)?;
    Ok(())
}

#[test]
fn a1_missing_pixels_recovers_owned_entry_and_preserves_unowned_orphan() -> Result<()> {
    let test = root("a1-missing");
    let image = test.join("own.png");
    let cache_root = test.join("tiles");
    fs::create_dir(&cache_root)?;
    generate(&image, 33, 17)?;
    let orphan = cache_root.join("cache-orphan");
    fs::create_dir(&orphan)?;
    fs::write(orphan.join("index.bin"), MAGIC)?;
    let broken = cache_root.join("cache-owned-missing");
    owned(&broken, MAGIC, None)?;
    let (cache, report) = build_or_open(&image, &cache_root, CacheBudget::default(), || true)?;
    assert!(!report.cache_hit);
    check_tiles(&cache)?;
    assert!(!broken.exists(), "有归属的残缺条目应被回收");
    assert_eq!(fs::read(orphan.join("index.bin"))?, MAGIC);
    let target = cache.directory.clone();
    drop(cache);
    fs::remove_file(target.join("pixels.bin"))?;
    fs::write(target.join("index.bin"), MAGIC)?;
    let (cache, report) = build_or_open(&image, &cache_root, CacheBudget::default(), || true)?;
    assert!(!report.cache_hit, "当前目标缺文件也必须能够重建");
    check_tiles(&cache)?;
    drop(cache);
    fs::remove_dir_all(test)?;
    Ok(())
}

#[test]
fn a1_bad_header_payload_is_charged_and_evicted_under_test_cap() -> Result<()> {
    let test = root("a1-accounting");
    let image = test.join("own.png");
    let cache_root = test.join("tiles");
    fs::create_dir(&cache_root)?;
    generate(&image, 33, 17)?;
    let bad = cache_root.join("cache-bad-header");
    owned(&bad, b"BADINDEX", Some(8 * 1024 * 1024))?;
    let unrelated = cache_root.join("cache-unrelated");
    fs::create_dir(&unrelated)?;
    fs::write(unrelated.join("keep"), b"user data")?;
    let budget = CacheBudget {
        disk_bytes: 1024 * 1024,
        ..Default::default()
    };
    let (cache, report) = build_or_open(&image, &cache_root, budget, || true)?;
    assert!(!report.cache_hit);
    assert!(!bad.exists(), "损坏索引不能让已归属的 8 MiB 逃逸计费");
    let actual = fs::metadata(cache.directory.join("pixels.bin"))?.len()
        + fs::metadata(cache.directory.join("index.bin"))?.len();
    assert!(actual <= budget.disk_bytes);
    assert_eq!(
        managed_bytes(&managed_entries(&cache_root, &|| true)?)?,
        actual,
        "受管理载荷必须与磁盘实际 index+pixels 一致，不能自相抵消"
    );
    assert_eq!(fs::read(unrelated.join("keep"))?, b"user data");
    check_tiles(&cache)?;
    println!(
        "A1 bad-header actual_managed_payload={actual} cap={}",
        budget.disk_bytes
    );
    drop(cache);
    fs::remove_dir_all(test)?;
    Ok(())
}

#[test]
fn a1_ownership_unknown_members_and_current_target_are_protected() -> Result<()> {
    let test = root("a1-ownership");
    let image = test.join("own.png");
    let cache_root = test.join("tiles");
    fs::create_dir(&cache_root)?;
    generate(&image, 33, 17)?;
    for (name, marker) in [
        ("cache-no-owner", None),
        ("cache-wrong-owner", Some(b"OTHERAPP".as_slice())),
        ("cache-long-owner", Some(b"PXSTILE1X".as_slice())),
    ] {
        let path = cache_root.join(name);
        fs::create_dir(&path)?;
        fs::write(path.join("index.bin"), MAGIC)?;
        fs::write(path.join("pixels.bin"), b"preserve")?;
        if let Some(marker) = marker {
            fs::write(path.join("owner"), marker)?;
        }
    }
    let unknown = cache_root.join("unrelated");
    owned(&unknown, MAGIC, Some(8))?;
    let extra = cache_root.join("cache-extra-member");
    owned(&extra, b"BADINDEX", Some(16))?;
    fs::create_dir(extra.join("user-folder"))?;
    fs::write(extra.join("user-folder/keep"), b"do not recurse")?;
    assert!(remove_inactive(&extra).is_err());
    assert_eq!(fs::metadata(extra.join("pixels.bin"))?.len(), 16);
    let (cache, _) = build_or_open(&image, &cache_root, CacheBudget::default(), || true)?;
    check_tiles(&cache)?;
    for name in ["cache-no-owner", "cache-wrong-owner", "cache-long-owner"] {
        assert_eq!(
            fs::read(cache_root.join(name).join("pixels.bin"))?,
            b"preserve"
        );
    }
    assert_eq!(fs::read(extra.join("user-folder/keep"))?, b"do not recurse");
    assert!(unknown.join("owner").is_file());
    let target = cache.directory.clone();
    drop(cache);
    fs::write(target.join("owner"), b"OTHERAPP")?;
    fs::write(target.join("index.bin"), b"BADINDEX")?;
    let before = fs::read(target.join("pixels.bin"))?;
    let error = build_or_open(&image, &cache_root, CacheBudget::default(), || true).unwrap_err();
    assert!(error.to_string().contains("无法安全替换"));
    assert_eq!(fs::read(target.join("owner"))?, b"OTHERAPP");
    assert_eq!(fs::read(target.join("pixels.bin"))?, before);
    fs::remove_dir_all(test)?;
    Ok(())
}

#[test]
fn a1_active_reader_and_delete_denial_remain_charged() -> Result<()> {
    let test = root("a1-pinned");
    let image = test.join("own.png");
    let other = test.join("other.png");
    let cache_root = test.join("tiles");
    generate(&image, 33, 17)?;
    generate(&other, 31, 19)?;
    let (cache, _) = build_or_open(&image, &cache_root, CacheBudget::default(), || true)?;
    let directory = cache.directory.clone();
    let old_bytes = fs::metadata(directory.join("index.bin"))?.len() + cache.plan.bytes;
    assert!(remove_inactive(&directory).is_err());
    check_tiles(&cache)?;
    assert_eq!(fs::read(directory.join("owner"))?, MAGIC);
    let tight = CacheBudget {
        disk_bytes: old_bytes,
        ..Default::default()
    };
    let other_size = stream::inspect(&other)?;
    let other_plan = Plan::new(other_size, tight.tile_edge)?;
    let other_stamp = Stamp::read(&other)?;
    let other_needed = other_plan
        .bytes
        .checked_add(encode(&other_stamp, &other_plan)?.len() as u64)
        .context("other 缓存需求溢出")?;
    assert!(
        other_needed <= tight.disk_bytes,
        "tight 预算必须足以单独容纳 other 的新缓存，拒绝原因才只能是保留旧缓存后的容量不足"
    );
    let error = build_or_open(&other, &cache_root, tight, || true).unwrap_err();
    assert!(
        error.to_string().contains("容量不足"),
        "活动 Reader 保留旧缓存后必须因容量不足拒绝：{error:#}"
    );
    assert_eq!(
        managed_bytes(&managed_entries(&cache_root, &|| true)?)?,
        old_bytes
    );
    check_tiles(&cache)?;
    fs::write(directory.join("index.bin"), b"BADINDEX")?;
    let untouched_partial = cache_root.join(".build-do-not-clean-after-refused-target");
    owned(&untouched_partial, MAGIC, None)?;
    let error = build_or_open(&image, &cache_root, CacheBudget::default(), || true).unwrap_err();
    assert!(error.to_string().contains("无法安全替换"));
    assert!(
        untouched_partial.join("owner").exists(),
        "目标拒绝替换后不能继续删除其他缓存"
    );
    assert_eq!(fs::read(directory.join("index.bin"))?, b"BADINDEX");
    assert_eq!(fs::read(directory.join("owner"))?, MAGIC);
    check_tiles(&cache)?;
    drop(cache);
    let bad = cache_root.join("cache-denied");
    owned(&bad, b"BADINDEX", Some(8 * 1024 * 1024))?;
    let pixels = bad.join("pixels.bin");
    let original_permissions = fs::metadata(&pixels)?.permissions();
    let mut permissions = original_permissions.clone();
    permissions.set_readonly(true);
    fs::set_permissions(&pixels, permissions.clone())?;
    let denied = build_or_open(
        &other,
        &cache_root,
        CacheBudget {
            disk_bytes: 1024 * 1024,
            ..Default::default()
        },
        || true,
    );
    assert!(denied.unwrap_err().to_string().contains("容量不足"));
    assert_eq!(fs::metadata(&pixels)?.len(), 8 * 1024 * 1024);
    assert_eq!(fs::read(bad.join("index.bin"))?, b"BADINDEX");
    assert_eq!(
        managed_bytes(&managed_entries(&cache_root, &|| true)?)?,
        8 * 1024 * 1024 + 8
    );
    // 只恢复本测试创建文件的属性，不改变系统权限或用户缓存。
    fs::set_permissions(&pixels, original_permissions)?;
    let (cache, _) = build_or_open(
        &other,
        &cache_root,
        CacheBudget {
            disk_bytes: 1024 * 1024,
            ..Default::default()
        },
        || true,
    )?;
    check_tiles(&cache)?;
    assert!(!bad.exists());
    drop(cache);
    fs::remove_dir_all(test)?;
    Ok(())
}

#[test]
fn a1_metadata_faults_overflow_and_scan_cancel_fail_closed() -> Result<()> {
    let test = root("a1-metadata");
    let cache_root = test.join("tiles");
    fs::create_dir(&cache_root)?;
    let bad = cache_root.join("cache-fault");
    owned(&bad, b"BADINDEX", Some(128))?;
    for code in [5, 1117] {
        let error = managed_entries_with(&cache_root, &|| true, |_| {
            Err(io::Error::from_raw_os_error(code).into())
        })
        .err()
        .context("必须保留查询错误")?;
        assert_eq!(
            error
                .downcast_ref::<io::Error>()
                .and_then(|e| e.raw_os_error()),
            Some(code)
        );
        assert_eq!(fs::metadata(bad.join("pixels.bin"))?.len(), 128);
    }
    assert!(managed_entries_with(&cache_root, &|| true, |_| Ok(Some(u64::MAX))).is_err());
    assert!(managed_entries(&cache_root, &|| false).is_err());
    assert_eq!(fs::read(bad.join("owner"))?, MAGIC);
    fs::remove_dir_all(test)?;
    Ok(())
}

#[test]
fn a1_partial_delete_failure_recounts_remaining_bytes_before_admission() -> Result<()> {
    let test = root("a1-partial-delete");
    let image = test.join("own.png");
    let cache_root = test.join("tiles");
    fs::create_dir(&cache_root)?;
    generate(&image, 33, 17)?;
    let bad = cache_root.join("cache-partial-delete");
    owned(&bad, b"BADINDEX", Some(8 * 1024 * 1024))?;
    let count = std::cell::Cell::new(0);
    let result = remove_inactive_with(&bad, |file| {
        count.set(count.get() + 1);
        if count.get() == 2 {
            return Err(io::Error::from_raw_os_error(5).into());
        }
        delete_handle(file)
    });
    assert!(result.is_err());
    assert!(!bad.join("index.bin").exists(), "第一项删除已经真实发生");
    assert_eq!(
        fs::read(bad.join("owner"))?,
        MAGIC,
        "载荷尚存时必须保留归属"
    );
    assert_eq!(fs::metadata(bad.join("pixels.bin"))?.len(), 8 * 1024 * 1024);
    let reader = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(bad.join("pixels.bin"))?;
    let budget = CacheBudget {
        disk_bytes: 1024 * 1024,
        ..Default::default()
    };
    assert!(
        build_or_open(&image, &cache_root, budget, || true)
            .unwrap_err()
            .to_string()
            .contains("容量不足")
    );
    assert_eq!(
        managed_bytes(&managed_entries(&cache_root, &|| true)?)?,
        8 * 1024 * 1024
    );
    drop(reader);
    let (cache, _) = build_or_open(&image, &cache_root, budget, || true)?;
    check_tiles(&cache)?;
    assert!(!bad.exists());
    drop(cache);
    fs::remove_dir_all(test)?;
    Ok(())
}

#[test]
fn a1_partial_cleanup_lease_and_post_failure_reopen() -> Result<()> {
    let test = root("a1-lease");
    let image = test.join("own.png");
    let cache_root = test.join("tiles");
    fs::create_dir(&cache_root)?;
    generate(&image, 33, 17)?;
    let partial = cache_root.join(".build-interrupted");
    owned(&partial, b"incomplete", Some(128))?;
    let lease = OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(cache_root.join("builder.lock"))?;
    let error = build_or_open(&image, &cache_root, CacheBudget::default(), || true).unwrap_err();
    assert!(error.to_string().contains("正忙"));
    assert_eq!(fs::metadata(partial.join("pixels.bin"))?.len(), 128);
    drop(lease);
    let (cache, report) = build_or_open(&image, &cache_root, CacheBudget::default(), || true)?;
    assert!(!report.cache_hit);
    assert!(!partial.exists());
    check_tiles(&cache)?;
    drop(cache);
    let (cache, report) = build_or_open(&image, &cache_root, CacheBudget::default(), || true)?;
    assert!(report.cache_hit);
    check_tiles(&cache)?;
    drop(cache);
    fs::remove_dir_all(test)?;
    Ok(())
}

#[test]
fn a1_junctions_and_pinned_directory_replacement_are_rejected() -> Result<()> {
    // mklink 会把未规范化路径中的 / 当作开关；只规范化本测试新建的目录。
    let test = fs::canonicalize(root("a1-reparse"))?;
    let image = test.join("own.png");
    let cache_root = test.join("tiles");
    let outside = test.join("outside");
    fs::create_dir(&cache_root)?;
    owned(&outside, MAGIC, Some(24))?;
    generate(&image, 33, 17)?;
    let junction = cache_root.join("cache-junction");
    let output = std::process::Command::new("cmd")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .output()?;
    ensure!(
        output.status.success(),
        "本机未能创建 junction，此验证未完成：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_ne!(
        fs::symlink_metadata(&junction)?.file_attributes() & REPARSE_POINT,
        0
    );
    assert!(remove_inactive(&junction).is_err());
    let pinned = pin_directory(&outside, false)?;
    let error = fs::rename(&outside, test.join("replacement")).unwrap_err();
    assert_eq!(
        error.raw_os_error(),
        Some(32),
        "目录句柄必须实际阻止检查后替换"
    );
    drop(pinned);
    let (cache, _) = build_or_open(&image, &cache_root, CacheBudget::default(), || true)?;
    check_tiles(&cache)?;
    assert_eq!(fs::metadata(outside.join("pixels.bin"))?.len(), 24);
    assert!(junction.exists());
    drop(cache);
    let member_entry = cache_root.join("cache-member-junction");
    owned(&member_entry, MAGIC, None)?;
    let member_link = member_entry.join("pixels.bin");
    let output = std::process::Command::new("cmd")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&member_link)
        .arg(&outside)
        .output()?;
    ensure!(
        output.status.success(),
        "无法创建成员 junction：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(remove_inactive(&member_entry).is_err());
    assert!(
        managed_entries(&cache_root, &|| true).is_err(),
        "不能跟随成员链接或以零字节漏计"
    );
    assert_eq!(fs::read(member_entry.join("owner"))?, MAGIC);
    fs::remove_dir(&member_link)?;
    // 仅移除本测试建立的链接本身，外部目标必须保持完整。
    fs::remove_dir(&junction)?;
    assert_eq!(fs::metadata(outside.join("pixels.bin"))?.len(), 24);
    fs::remove_dir_all(test)?;
    println!("A1 junction creation and no-follow protection exercised (not skipped)");
    Ok(())
}

/// 两条真实有效缓存按显式目录 mtime 区分；配额恰好保留较新缓存与本次新构建。
#[test]
fn a1_oldest_cache_is_evicted_when_exact_cap_keeps_newer_and_new() -> Result<()> {
    let test = root("a1-oldest-first");
    let old_image = test.join("old.png");
    let new_image = test.join("new.png");
    let third_image = test.join("third.png");
    let cache_root = test.join("tiles");
    fs::create_dir(&cache_root)?;
    generate(&old_image, 33, 17)?;
    generate(&new_image, 33, 17)?;
    generate(&third_image, 31, 19)?;

    let (cache, _) = build_or_open(&old_image, &cache_root, CacheBudget::default(), || true)?;
    let old_directory = cache.directory.clone();
    drop(cache);
    let (cache, _) = build_or_open(&new_image, &cache_root, CacheBudget::default(), || true)?;
    let new_directory = cache.directory.clone();
    drop(cache);
    assert_ne!(old_directory, new_directory);
    let old_payload = cached_payload(&old_directory)?;
    let new_payload = cached_payload(&new_directory)?;
    assert_eq!(
        old_payload, new_payload,
        "同几何与同长度源文件名应使两条缓存载荷等大"
    );

    let base = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    set_directory_modified(&old_directory, base)?;
    set_directory_modified(&new_directory, base + std::time::Duration::from_secs(3_600))?;

    let third_size = stream::inspect(&third_image)?;
    let third_plan = Plan::new(third_size, CacheBudget::default().tile_edge)?;
    let third_stamp = Stamp::read(&third_image)?;
    let needed = third_plan
        .bytes
        .checked_add(encode(&third_stamp, &third_plan)?.len() as u64)
        .context("第三张缓存需求溢出")?;
    let cap = new_payload.checked_add(needed).context("测试配额溢出")?;
    let budget = CacheBudget {
        disk_bytes: cap,
        ..Default::default()
    };
    let (cache, report) = build_or_open(&third_image, &cache_root, budget, || true)?;
    assert!(!report.cache_hit);
    check_tiles(&cache)?;
    let third_directory = cache.directory.clone();
    let third_payload = cached_payload(&third_directory)?;
    assert_eq!(third_payload, needed, "新缓存落盘载荷应恰好等于准入需求");
    drop(cache);

    assert!(!old_directory.exists(), "最旧缓存必须先被淘汰");
    assert!(new_directory.exists(), "较新缓存必须保留");
    assert_eq!(cached_payload(&new_directory)?, new_payload);
    assert_eq!(new_payload + third_payload, cap);

    let (cache, report) = build_or_open(&third_image, &cache_root, budget, || true)?;
    assert!(report.cache_hit);
    assert!(report.scan.is_none());
    check_tiles(&cache)?;
    drop(cache);
    let (cache, report) = build_or_open(&new_image, &cache_root, CacheBudget::default(), || true)?;
    assert!(report.cache_hit, "保留的较新缓存仍需暖打开命中");
    assert!(report.scan.is_none(), "暖打开不应重新扫描源图");
    check_tiles(&cache)?;
    drop(cache);

    let managed = managed_bytes(&managed_entries(&cache_root, &|| true)?)?;
    assert_eq!(
        managed,
        actual_cache_payload(&cache_root)?,
        "受管理字节必须与磁盘实际 index+pixels 完全一致"
    );
    assert_eq!(managed, cap, "精确配额下受管理字节必须恰好等于 cap");
    fs::remove_dir_all(test)?;
    Ok(())
}

/// 只比较本次小图的冷构建、暖打开与故障恢复，不读取用户缓存或推断跨版本性能。
#[test]
#[ignore = "A1 Windows Release 恢复诊断；使用独立 PIXSTRIDE_TEST_ROOT"]
fn release_a1_recovery_probe() -> Result<()> {
    let test = root("a1-release");
    let image = test.join("own.png");
    let cache_root = test.join("tiles");
    generate(&image, 1025, 65)?;
    let budget = CacheBudget {
        disk_bytes: 1024 * 1024,
        ..Default::default()
    };
    let (cache, cold) = build_or_open(&image, &cache_root, budget, || true)?;
    check_tiles(&cache)?;
    drop(cache);
    let mut warm = Vec::new();
    for _ in 0..30 {
        let (cache, report) = build_or_open(&image, &cache_root, budget, || true)?;
        assert!(report.cache_hit);
        assert!(report.scan.is_none());
        warm.push(report.elapsed.as_micros());
        drop(cache);
    }
    warm.sort_unstable();
    owned(
        &cache_root.join("cache-bad-header"),
        b"BADINDEX",
        Some(8 * 1024 * 1024),
    )?;
    owned(&cache_root.join("cache-missing"), MAGIC, None)?;
    generate(&image, 1026, 65)?;
    let (cache, recovered) = build_or_open(&image, &cache_root, budget, || true)?;
    check_tiles(&cache)?;
    let actual = managed_bytes(&managed_entries(&cache_root, &|| true)?)?;
    assert!(actual <= budget.disk_bytes);
    assert!(!cache_root.join("cache-bad-header").exists());
    assert!(!cache_root.join("cache-missing").exists());
    println!(
        "A1 RELEASE cold_us={} warm_n=30 warm_min_us={} warm_p50_us={} warm_p95_us={} warm_max_us={} recovery_us={} managed_bytes={actual} test_cap={}; warm OS cache, no cross-version A/B or display latency",
        cold.elapsed.as_micros(),
        warm[0],
        warm[14],
        warm[28],
        warm[29],
        recovered.elapsed.as_micros(),
        budget.disk_bytes
    );
    drop(cache);
    fs::remove_dir_all(test)?;
    Ok(())
}
