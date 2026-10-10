use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use phaedrus_dom::Document;
use phaedrus_luau::datastore::{FailureCode, Operation};
use phaedrus_luau::{DataStoreConfig, DataStoreEmulator, OutputLevel, Runtime, SharedDataStore, SharedOutput};

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("phaedrus-ds-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn config() -> DataStoreConfig {
    DataStoreConfig { enabled: true, latency_ms: 0, latency_jitter_ms: 0, enforce_budgets: false, ..Default::default() }
}

fn runtime(emulator: Option<SharedDataStore>) -> (Arc<Mutex<Document>>, SharedOutput, Runtime) {
    let doc = Arc::new(Mutex::new(Document::new_place()));
    let out: SharedOutput = Default::default();
    let rt = Runtime::new(doc.clone(), out.clone(), true).unwrap();
    rt.set_datastore(emulator);
    (doc, out, rt)
}

/// Runs `source` and steps the VM until it prints "done" (or gives up).
fn run(source: &str, emulator: Option<SharedDataStore>) -> Vec<(OutputLevel, String)> {
    let (doc, out, rt) = runtime(emulator);
    rt.exec(source, "test");
    for _ in 0..200 {
        if out.lock().iter().any(|l| l.text == "done") || out.lock().iter().any(|l| l.level == OutputLevel::Error) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        let changes = doc.lock().take_changes();
        rt.step(0.016, &changes);
    }
    let lines = out.lock().iter().map(|l| (l.level, l.text.clone())).collect();
    lines
}

fn texts(lines: &[(OutputLevel, String)]) -> Vec<String> {
    lines.iter().filter(|(l, _)| *l != OutputLevel::Warn).map(|(_, t)| t.clone()).collect()
}

#[test]
fn disabled_service_errors_like_unpublished_place() {
    let lines = run(
        r#"
        local DSS = game:GetService("DataStoreService")
        local ok, err = pcall(function() return DSS:GetDataStore("PlayerData") end)
        print(ok, err)
        print("done")
        "#,
        None,
    );
    assert_eq!(texts(&lines), vec!["false You must publish this place to the web to access DataStore.", "done"]);
}

#[test]
fn standard_store_round_trip_and_persistence() {
    let root = temp_root("roundtrip");
    let place = Path::new("C:/places/My Place.rbxl");
    let emu = DataStoreEmulator::new(root.clone(), config());
    emu.set_place(Some(place)).unwrap();
    let lines = run(
        r#"
        local DSS = game:GetService("DataStoreService")
        local store = DSS:GetDataStore("PlayerData")
        print(typeof(store), store.ClassName, store.Name, store:IsA("GlobalDataStore"))
        local options = Instance.new("DataStoreSetOptions")
        options:SetMetadata({ source = "test" })
        local version = store:SetAsync("Player_1", { coins = 10, items = { "sword", "shield" } }, { 1 }, options)
        print(type(version))
        local value, info = store:GetAsync("Player_1")
        print(value.coins, value.items[2], info.Version == version, info:GetUserIds()[1], info:GetMetadata().source)
        local updated = store:UpdateAsync("Player_1", function(old, keyInfo)
            old.coins += 5
            return old
        end)
        print(updated.coins)
        print(store:IncrementAsync("Visits", 3), store:IncrementAsync("Visits"))
        local pages = store:ListKeysAsync()
        local names = {}
        for _, k in pages:GetCurrentPage() do table.insert(names, k.KeyName) end
        table.sort(names)
        print(table.concat(names, ","), pages.IsFinished)
        local versions = store:ListVersionsAsync("Player_1", Enum.SortDirection.Descending, nil, nil, 10)
        print(#versions:GetCurrentPage())
        local removed = store:RemoveAsync("Visits")
        print(removed, store:GetAsync("Visits"))
        local ok, err = pcall(function() store:SetAsync("bad", { Vector3.new() }) end)
        print(ok, err)
        local ok2, err2 = pcall(function() store:GetAsync("") end)
        print(err2)
        local ok3, err3 = pcall(function()
            store:UpdateAsync("Player_1", function(v) task.wait() return v end)
        end)
        print(err3)
        print("done")
        "#,
        Some(emu.clone()),
    );
    assert_eq!(
        texts(&lines),
        vec![
            "Instance DataStore PlayerData true",
            "string",
            "10 shield true 1 test",
            "15",
            "3 4",
            "Player_1,Visits true",
            "2",
            "4 nil nil",
            "false 104: Cannot store Vector3 in data store. Data stores can only accept valid UTF-8 characters.",
            "101: Key name can't be empty.",
            "UpdateAsync transform function cannot yield",
            "done",
        ]
    );
    emu.flush().unwrap();
    let file = emu.data_file().unwrap();
    assert!(file.starts_with(&root));
    assert!(file.file_name().unwrap().to_string_lossy().starts_with("My_Place-"));

    // A new emulator for the same place sees the saved data.
    let emu2 = DataStoreEmulator::new(root.clone(), config());
    emu2.set_place(Some(place)).unwrap();
    let lines = run(
        r#"
        local store = game:GetService("DataStoreService"):GetDataStore("PlayerData")
        print(store:GetAsync("Player_1").coins)
        print("done")
        "#,
        Some(emu2),
    );
    assert_eq!(texts(&lines), vec!["15", "done"]);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn ordered_store_sorting() {
    let emu = DataStoreEmulator::new(temp_root("ordered"), config());
    emu.set_place(None).unwrap();
    let lines = run(
        r#"
        local store = game:GetService("DataStoreService"):GetOrderedDataStore("Leaderboard")
        for i, name in { "a", "b", "c", "d" } do store:SetAsync(name, i * 10) end
        store:IncrementAsync("a", 100)
        local pages = store:GetSortedAsync(false, 2)
        local out = {}
        for _, e in pages:GetCurrentPage() do table.insert(out, e.key .. "=" .. e.value) end
        pages:AdvanceToNextPageAsync()
        for _, e in pages:GetCurrentPage() do table.insert(out, e.key .. "=" .. e.value) end
        print(table.concat(out, " "), pages.IsFinished)
        print(pcall(function() store:SetAsync("x", 1.5) end))
        print("done")
        "#,
        Some(emu),
    );
    let t = texts(&lines);
    assert_eq!(t[0], "a=110 d=40 c=30 b=20 true");
    assert!(t[1].starts_with("false"), "{}", t[1]);
}

#[test]
fn simulated_failures() {
    let mut cfg = config();
    *cfg.rule_mut(Operation::SetAsync) = phaedrus_luau::datastore::FailureRule { enabled: true, code: FailureCode::InternalError500, chance: 100.0 };
    let emu = DataStoreEmulator::new(temp_root("fail"), cfg.clone());
    emu.set_place(None).unwrap();
    let lines = run(
        r#"
        local store = game:GetService("DataStoreService"):GetDataStore("PlayerData")
        print(pcall(store.SetAsync, store, "k", 1))
        print(pcall(store.GetAsync, store, "k"))
        print("done")
        "#,
        Some(emu.clone()),
    );
    let t = texts(&lines);
    assert!(t[0].starts_with("false 500:"), "{}", t[0]);
    assert_eq!(t[1], "true nil nil");
    assert!(lines.iter().any(|(l, s)| *l == OutputLevel::Warn && s.contains("simulated 500")));

    // Outage with a store filter: only matching stores fail.
    cfg.outage = true;
    cfg.outage_code = FailureCode::BadGateway502;
    cfg.store_filter = "Player*".into();
    emu.set_config(cfg);
    let lines = run(
        r#"
        local DSS = game:GetService("DataStoreService")
        print(pcall(function() return DSS:GetDataStore("PlayerData"):GetAsync("k") end))
        print(pcall(function() return DSS:GetDataStore("Other"):GetAsync("k") end))
        print("done")
        "#,
        Some(emu),
    );
    let t = texts(&lines);
    assert!(t[0].starts_with("false 502:"), "{}", t[0]);
    assert_eq!(t[1], "true nil nil");
}

#[test]
fn budget_queue_warns_and_waits() {
    let mut cfg = config();
    cfg.enforce_budgets = true;
    let emu = DataStoreEmulator::new(temp_root("budget"), cfg);
    emu.set_place(None).unwrap();
    let (doc, out, rt) = runtime(Some(emu));
    rt.exec(
        r#"
        local store = game:GetService("DataStoreService"):GetDataStore("S")
        print(game:GetService("DataStoreService"):GetRequestBudgetForRequestType(Enum.DataStoreRequestType.SetIncrementAsync))
        store:SetAsync("k", 1)
        store:SetAsync("k", 2)
        print("second write finished")
        "#,
        "test",
    );
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(5));
        let changes = doc.lock().take_changes();
        rt.step(0.016, &changes);
    }
    let lines: Vec<_> = out.lock().iter().map(|l| (l.level, l.text.clone())).collect();
    assert_eq!(lines[0].1, "60");
    // The second write to the same key within 6 seconds is queued.
    assert!(lines.iter().any(|(l, t)| *l == OutputLevel::Warn && t.contains("added to queue")), "{lines:?}");
    assert!(!lines.iter().any(|(_, t)| t == "second write finished"));
}
