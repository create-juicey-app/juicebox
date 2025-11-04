mod common;

use juicebox::state::{
    BanSubject, FileMeta, IpBan, check_storage_integrity, cleanup_expired,
    verify_user_entries_with_report,
};
use juicebox::util::now_secs;
use std::collections::HashMap;
use std::time::Duration;
use tokio::fs;

fn meta(owner_hash: String, expires: u64, original: &str) -> FileMeta {
    FileMeta {
        owner_hash,
        expires,
        original: original.to_string(),
        created: now_secs(),
        hash: "deadbeef".into(),
    }
}

#[tokio::test]
async fn admin_session_lifecycle() {
    let (state, _tmp) = common::setup_test_app();

    // Initially not admin
    assert!(!state.is_admin("tok").await);

    // Create a session and verify
    state.create_admin_session("tok".to_string()).await;
    assert!(state.is_admin("tok").await);

    // Expire it and clean up
    {
        let mut sessions = state.admin_sessions.write().await;
        sessions.insert("tok".to_string(), now_secs().saturating_sub(1));
    }
    state.cleanup_admin_sessions().await;
    assert!(!state.is_admin("tok").await);
}

#[tokio::test]
async fn bans_exact_hash_add_find_remove() {
    let (state, _tmp) = common::setup_test_app();

    let ip = "198.51.100.5";
    let hash = state
        .hash_ip_to_string(ip)
        .expect("hash must be derivable for valid IP");

    // Add the ban
    state
        .add_ban(IpBan {
            subject: BanSubject::Exact { hash: hash.clone() },
            label: Some("unit-test".into()),
            reason: "testing".into(),
            time: 0,
        })
        .await;

    // IP should be banned
    assert!(state.is_banned(ip).await);

    // Raw hash should also be recognized as banned input
    assert!(state.is_banned(&hash).await);

    // find_ban_for_input works both ways
    let found = state.find_ban_for_input(ip).await;
    assert!(found.is_some());
    let found_hash = state.find_ban_for_input(&hash).await;
    assert!(found_hash.is_some());

    // Remove ban
    state.remove_ban(found.unwrap().subject.key()).await;
    assert!(!state.is_banned(ip).await);
}

#[tokio::test]
async fn bans_network_scope_and_nonmember() {
    let (state, _tmp) = common::setup_test_app();

    // Build a network ban for 203.0.113.0/24 using an IP within it
    let test_ip: std::net::IpAddr = "203.0.113.7".parse().unwrap();
    let (version, prefix, net_hash) = state
        .hash_network_for_ip(&test_ip, 24)
        .expect("network hash available");

    state
        .add_ban(IpBan {
            subject: BanSubject::Network {
                hash: net_hash.clone(),
                prefix,
                version,
            },
            label: None,
            reason: "test-net".into(),
            time: 0,
        })
        .await;

    // Member of 203.0.113.0/24 should be banned
    assert!(state.is_banned("203.0.113.88").await);
    // Different network should not be banned
    assert!(!state.is_banned("203.0.114.1").await);

    // Remove ban via key and verify
    state.remove_ban(&net_hash).await;
    assert!(!state.is_banned("203.0.113.88").await);
}

#[tokio::test]
async fn cleanup_expired_removes_metadata_and_files() {
    let (state, _tmp) = common::setup_test_app();

    // Prepare a file with expired metadata
    let fname = "expired.bin".to_string();
    let owner = common::hash_fixture_ip("127.0.0.1");
    let past = now_secs().saturating_sub(10);

    // Insert metadata
    state
        .owners
        .insert(fname.clone(), meta(owner.clone(), past, "expired.bin"));

    // Create file on disk
    let path = state.upload_dir.join(&fname);
    fs::write(&path, b"stale").await.unwrap();
    assert!(path.exists());

    // Run cleanup
    cleanup_expired(&state).await;

    // Metadata removed and file deleted
    assert!(state.owners.get(&fname).is_none());
    assert!(!path.exists());
}

#[tokio::test]
async fn check_storage_integrity_removes_orphaned_metadata() {
    let (state, _tmp) = common::setup_test_app();

    // Insert metadata for a file that does not exist on disk
    let fname = "ghost.txt".to_string();
    let owner = common::hash_fixture_ip("10.0.0.1");
    let future = now_secs() + 3600;
    state
        .owners
        .insert(fname.clone(), meta(owner, future, "ghost.txt"));

    // Ensure file is missing then verify integrity
    let path = state.upload_dir.join(&fname);
    assert!(!path.exists());

    check_storage_integrity(&state).await;

    // The orphaned entry should be removed
    assert!(state.owners.get(&fname).is_none());
}

#[tokio::test]
async fn verify_user_entries_reconciles_with_disk() {
    let (state, _tmp) = common::setup_test_app();

    let owner_hash = common::hash_fixture_ip("127.0.0.1");
    let now = now_secs();

    // Memory has two entries for this owner: a.txt and b.txt
    let file_a = "a.txt".to_string();
    let file_b = "b.txt".to_string();
    state.owners.insert(
        file_a.clone(),
        meta(owner_hash.clone(), now + 1000, "old.txt"),
    );
    state.owners.insert(
        file_b.clone(),
        meta(owner_hash.clone(), now + 2000, "keep?"),
    );

    // Disk will contain:
    // - a.txt updated (different expires + original)
    // - c.txt added for this owner
    // - d.txt for a different owner (should be ignored)
    let file_c = "c.txt".to_string();
    let other_owner = common::hash_fixture_ip("192.0.2.1");

    let mut disk_map = HashMap::<String, FileMeta>::new();
    disk_map.insert(
        file_a.clone(),
        meta(owner_hash.clone(), now + 9999, "new.txt"),
    );
    disk_map.insert(
        file_c.clone(),
        meta(owner_hash.clone(), now + 3333, "c.txt"),
    );
    disk_map.insert("d.txt".into(), meta(other_owner, now + 1234, "d.txt"));

    // Persist disk metadata
    let json = serde_json::to_vec_pretty(&disk_map).unwrap();
    fs::write(&*state.metadata_path, json).await.unwrap();

    // Give filesystem a moment to ensure mtime ordering isn't equal on some FS
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Run reconciliation for the target owner
    let report = verify_user_entries_with_report(&state, &owner_hash)
        .await
        .expect("expected reconciliation to happen");

    // Validate report contents
    assert!(
        report.removed.contains(&file_b),
        "expected b.txt to be removed: {:?}",
        report.removed
    );
    assert!(
        report.updated.contains(&file_a),
        "expected a.txt to be updated: {:?}",
        report.updated
    );
    assert!(
        report.added.contains(&file_c),
        "expected c.txt to be added: {:?}",
        report.added
    );

    // Validate in-memory state reflects the disk
    let a = state.owners.get(&file_a).unwrap();
    let a_meta = a.value();
    assert_eq!(a_meta.original, "new.txt");
    assert!(a_meta.expires > now);

    assert!(state.owners.get(&file_b).is_none());

    let c = state.owners.get(&file_c).unwrap();
    let c_meta = c.value();
    assert_eq!(c_meta.original, "c.txt");
}
