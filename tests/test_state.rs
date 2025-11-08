mod common;

use juicebox::state::{
    BanSubject, FileMeta, IpBan, check_storage_integrity, cleanup_expired,
    verify_user_entries_with_report,
};
use juicebox::util::now_secs;
use std::collections::HashMap;
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
    fs::write(state.upload_dir.join(&file_a), b"existing-a")
        .await
        .unwrap();
    fs::write(state.upload_dir.join(&file_b), b"existing-b")
        .await
        .unwrap();

    // Disk will contain:
    // - a.txt updated (different expires + original)
    // - c.txt added for this owner
    // - d.txt for a different owner (should be ignored)
    let file_c = "c.txt".to_string();
    let other_owner = common::hash_fixture_ip("192.0.2.1");

    let mut serialized = Vec::new();
    serialized.push((
        file_a.clone(),
        serde_json::to_string(&meta(owner_hash.clone(), now + 9999, "new.txt")).unwrap(),
    ));
    serialized.push((
        file_c.clone(),
        serde_json::to_string(&meta(owner_hash.clone(), now + 3333, "c.txt")).unwrap(),
    ));
    serialized.push((
        "d.txt".to_string(),
        serde_json::to_string(&meta(other_owner, now + 1234, "d.txt")).unwrap(),
    ));

    fs::write(state.upload_dir.join(&file_c), b"existing-c")
        .await
        .unwrap();

    // Persist metadata into the key-value store to simulate disk state
    state
        .kv
        .replace_hash("owners", &serialized)
        .await
        .expect("kv write should succeed");

    // Run reconciliation for the target owner
    let report = verify_user_entries_with_report(&state, &owner_hash)
        .await
        .expect("expected reconciliation to happen");

    // Validate report contents
    assert!(
        report.updated.contains(&file_a),
        "expected a.txt to be updated: {:?}",
        report.updated
    );
    assert!(
        report.updated.contains(&file_b),
        "expected b.txt to be synced back to the store: {:?}",
        report.updated
    );
    assert!(
        report.added.contains(&file_c),
        "expected c.txt to be added: {:?}",
        report.added
    );
    assert!(
        report.removed.is_empty(),
        "did not expect removals but saw {:?}",
        report.removed
    );

    // Validate in-memory state reflects the disk
    let a = state.owners.get(&file_a).unwrap();
    let a_meta = a.value();
    assert_eq!(a_meta.original, "new.txt");
    assert!(a_meta.expires > now);

    assert!(state.owners.get(&file_b).is_some());

    let c = state.owners.get(&file_c).unwrap();
    let c_meta = c.value();
    assert_eq!(c_meta.original, "c.txt");

    let stored_entries = state.kv.load_hash("owners").await.unwrap();
    let mut stored_map = HashMap::new();
    for (fname, payload) in stored_entries {
        stored_map.insert(fname, serde_json::from_str::<FileMeta>(&payload).unwrap());
    }
    assert!(stored_map.contains_key(&file_a));
    assert!(stored_map.contains_key(&file_b));
    assert!(stored_map.contains_key(&file_c));
}

#[tokio::test]
async fn reconcile_flushes_missing_store_entries() {
    let (state, _tmp) = common::setup_test_app();
    let owner_hash = common::hash_fixture_ip("198.51.100.7");
    let fname = "recover.bin".to_string();
    let meta = meta(owner_hash.clone(), now_secs() + 3600, "recover.bin");

    state.owners.insert(fname.clone(), meta.clone());

    // Ensure key-value store currently lacks this entry.
    state
        .kv
        .replace_hash("owners", &[])
        .await
        .expect("kv clear should succeed");

    let report = verify_user_entries_with_report(&state, &owner_hash)
        .await
        .expect("reconciliation should return a report");
    assert!(
        report.updated.contains(&fname),
        "expected updated list to include {} but got {:?}",
        fname,
        report.updated
    );
    assert!(
        state.owners.get(&fname).is_some(),
        "owner entry was unexpectedly removed from memory"
    );

    let stored_entries = state.kv.load_hash("owners").await.unwrap();
    let stored = stored_entries
        .into_iter()
        .find(|(key, _)| key == &fname)
        .map(|(_, payload)| serde_json::from_str::<FileMeta>(&payload).unwrap());
    let stored = stored.expect("entry should be persisted to kv store");
    assert_eq!(stored.owner_hash, owner_hash);
    assert_eq!(stored.original, meta.original);
}

#[tokio::test]
async fn reconcile_cleans_stale_store_entries() {
    let (state, _tmp) = common::setup_test_app();
    let owner_hash = common::hash_fixture_ip("203.0.113.9");
    let fname = "orphan.bin".to_string();
    let payload =
        serde_json::to_string(&meta(owner_hash.clone(), now_secs() + 3600, "orphan.bin")).unwrap();

    state
        .kv
        .replace_hash("owners", &[(fname.clone(), payload)])
        .await
        .expect("kv populate should succeed");

    let report = verify_user_entries_with_report(&state, &owner_hash)
        .await
        .expect("reconciliation should return a report");
    assert!(
        report.removed.contains(&fname),
        "expected removed list to include {} but got {:?}",
        fname,
        report.removed
    );

    let stored_entries = state.kv.load_hash("owners").await.unwrap();
    assert!(
        stored_entries.into_iter().all(|(key, _)| key != fname),
        "stale kv entry should have been removed"
    );
}
