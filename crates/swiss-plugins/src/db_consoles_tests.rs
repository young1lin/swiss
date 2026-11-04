//! Encoded frames through the real pump, without sockets (SPEC §bus.requests).
use super::*;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::mpsc;

struct Channel { rx: mpsc::Receiver<swiss_bus::IoMsg>, tx: mpsc::Sender<swiss_bus::IoMsg> }
impl swiss_bus::Io for Channel {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<swiss_bus::IoMsg>> + Send + '_>> {
        Box::pin(async move { self.rx.recv().await })
    }
    fn send(&mut self, msg: swiss_bus::IoMsg) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        Box::pin(async move { self.tx.send(msg).await.is_ok() })
    }
}

pub(crate) struct Peer {
    tx: mpsc::Sender<swiss_bus::IoMsg>, rx: mpsc::Receiver<swiss_bus::IoMsg>, seq: u32,
    events: std::collections::VecDeque<v1::Frame>,
}
impl Peer {
    pub(crate) async fn open(bus: Arc<Bus>) -> Self {
        let (tx, rx) = mpsc::channel(32);
        let (out, incoming) = mpsc::channel(32);
        tokio::spawn(swiss_bus::run(bus, Channel { rx, tx: out }));
        let mut peer = Self { tx, rx: incoming, seq: 1, events: Default::default() };
        let welcome = peer.request_at(0, v1::BusSub::Link as u32, v1::LinkId::Hello as u32,
            v1::Hello { protocol: 1, page_id: "page-a".into(), build: "test".into() }.encode_to_vec()).await;
        assert!(welcome.fault.is_none());
        peer
    }
    async fn next(&mut self) -> v1::Frame {
        match self.rx.recv().await.expect("link frame") {
            swiss_bus::IoMsg::Binary(bytes) => v1::Frame::decode(bytes.as_slice()).expect("frame"),
            other => panic!("expected binary, got {other:?}"),
        }
    }
    async fn request_at(&mut self, module: u32, sub: u32, id: u32, body: Vec<u8>) -> v1::Frame {
        let seq = self.seq;
        self.seq += 1;
        self.tx.send(swiss_bus::IoMsg::Binary(v1::Frame {
            module, sub, id, kind: v1::Kind::Request as i32, seq, body, ..Default::default()
        }.encode_to_vec())).await.expect("send");
        loop {
            let frame = self.next().await;
            if frame.kind == v1::Kind::Event as i32 { self.events.push_back(frame); continue; }
            assert_eq!(frame.reply_to, seq);
            return frame;
        }
    }
    pub(crate) async fn request(&mut self, id: v1::ConsolesId, body: impl Message) -> v1::Frame {
        self.raw(id, body.encode_to_vec()).await
    }
    async fn raw(&mut self, id: v1::ConsolesId, body: Vec<u8>) -> v1::Frame {
        self.request_at(v1::Module::Data as u32, v1::DataSub::Consoles as u32, id as u32, body).await
    }
    pub(crate) async fn subscribe(&mut self, conn: &str) {
        let answer = self.request_at(0, v1::BusSub::Topic as u32, v1::TopicId::Subscribe as u32,
            v1::Topics { topics: vec![v1::Topic { module: v1::Module::Data as u32,
                sub: v1::DataSub::Consoles as u32, key: conn.into() }] }.encode_to_vec()).await;
        assert!(answer.fault.is_none());
    }
    pub(crate) async fn event(&mut self) -> v1::Frame {
        if let Some(event) = self.events.pop_front() { return event; }
        let event = self.next().await;
        assert_eq!(event.kind, v1::Kind::Event as i32);
        event
    }
    pub(crate) async fn list(&mut self, conn: &str) -> v1::ConsoleList {
        let frame = self.request(v1::ConsolesId::ConsolesList,
            v1::ConsolesPage { conn: conn.into(), after: String::new() }).await;
        assert!(frame.fault.is_none(), "{:?}", frame.fault);
        v1::ConsoleList::decode(frame.body.as_slice()).expect("list")
    }
}

pub(crate) struct Catalog;
impl swiss_host::services::catalog::ConnectionCatalog for Catalog {
    fn list(&self) -> Vec<swiss_host::services::catalog::ConnectionInfo> { Vec::new() }
    fn lease(&self, id: &str, holder: &str) -> Result<swiss_host::services::catalog::ConnectionLease, swiss_host::services::catalog::CatalogError> {
        if id != "mm" && id != "mm2" {
            return Err(swiss_host::services::catalog::CatalogError::Unknown("unknown connection".into()));
        }
        Ok(swiss_host::services::catalog::LeaseTracker::new().grant(id, holder, "pg", swiss_host::dbbrowser::BrowserFlavor::None))
    }
}

fn record(id: &str, open: bool) -> Console {
    Console { id: id.into(), tab: "Report".into(), sql: "SELECT 1".into(), open, ..Default::default() }
}

#[tokio::test]
async fn list_pages_fit_two_mib_without_losing_or_repeating_records() {
    let root = std::env::temp_dir().join(format!("swiss-bus-pages-{}", swiss_core::util::random_hex(8)));
    let services = RuntimeServices::new(swiss_host::services::BootInfo::default());
    services.catalog.register(Arc::new(Catalog), "test").expect("catalog");
    let store = ConsoleStore::at(root.clone(), services.bus.clone());
    let _guard = store.register(services.clone());
    let mut peer = Peer::open(services.bus.clone()).await;
    for id in ["c", "a", "b"] {
        store.save("mm", Console { sql: "x".repeat(1_500_000), ..record(id, true) }).expect("save");
    }
    let mut after = String::new();
    let mut ids = Vec::new();
    let mut pages = 0;
    loop {
        let frame = peer.request(v1::ConsolesId::ConsolesList, v1::ConsolesPage { conn: "mm".into(), after: after.clone() }).await;
        assert!(frame.fault.is_none());
        let page = v1::ConsoleList::decode(frame.body.as_slice()).expect("page");
        assert!(page.encoded_len() <= swiss_bus::LIST_PAGE_MAX);
        assert!(page.encoded_len() < swiss_bus::MAX_FRAME);
        pages += 1;
        ids.extend(page.consoles.into_iter().map(|c| c.id));
        if page.next.is_empty() { break; }
        after = page.next;
        // A write between pages cannot offset the id cursor (SPEC §bus.topics).
        if pages == 1 {
            store.remove("mm", "a", Some(1), "").expect("remove behind cursor");
            store.save("mm", record("d", true)).expect("insert ahead of cursor");
        }
    }
    assert!(pages > 1);
    assert_eq!(ids, ["a", "b", "c", "d"]);
    assert_eq!(peer.list("mm2").await, v1::ConsoleList::default());
    std::fs::remove_dir_all(root).expect("scratch cleanup");
}

#[tokio::test]
async fn console_bus_writes_publish_origin_revisions_and_refuse_stale_edits() {
    let root = std::env::temp_dir().join(format!("swiss-bus-write-{}", swiss_core::util::random_hex(8)));
    let services = RuntimeServices::new(swiss_host::services::BootInfo::default());
    // The catalog stub supplies real leases, with no engine or socket (SPEC §host.seats).
    services.catalog.register(Arc::new(Catalog), "test").expect("catalog");
    let store = ConsoleStore::at(root.clone(), services.bus.clone());
    let _guard = store.register(services.clone());
    let mut peer = Peer::open(services.bus.clone()).await;
    peer.subscribe("mm").await;
    let put = |c: Console| v1::ConsolePut { conn: "mm".into(), console: Some(c.into()), mode: v1::SaveMode::Auto as i32 };
    let first = peer.request(v1::ConsolesId::ConsolesPut, v1::ConsolePut { mode: v1::SaveMode::Create as i32, ..put(record("a", true)) }).await;
    assert!(first.fault.is_none());
    let saved = v1::ConsoleSaved::decode(first.body.as_slice()).expect("saved").console.expect("console");
    assert_eq!(saved.revision, 1);
    let event = peer.event().await;
    assert_eq!((event.id, event.origin.as_str(), event.key.as_str()), (v1::ConsolesId::ConsolesChanged as u32, "page-a", "mm"));
    assert_eq!(v1::ConsoleSaved::decode(event.body.as_slice()).expect("changed").console, Some(saved.clone()));
    let exists = peer.request(v1::ConsolesId::ConsolesPut, v1::ConsolePut { mode: v1::SaveMode::Create as i32, ..put(saved.clone().into()) }).await;
    assert_eq!(exists.fault.expect("create existing").code, "console.exists");
    let missing = peer.request(v1::ConsolesId::ConsolesPut, v1::ConsolePut { mode: v1::SaveMode::Update as i32, ..put(record("missing", true)) }).await;
    assert_eq!(missing.fault.expect("update missing").code, "console.removed");
    let stale = peer.request(v1::ConsolesId::ConsolesPut, put(record("a", true))).await;
    assert_eq!(stale.fault.expect("stale").code, "console.changed");
    let second = peer.request(v1::ConsolesId::ConsolesPut, v1::ConsolePut { mode: v1::SaveMode::Update as i32, ..put(Console { sql: "SELECT 2".into(), ..saved.clone().into() }) }).await;
    assert!(second.fault.is_none());
    let event = peer.event().await;
    assert_eq!(v1::ConsoleSaved::decode(event.body.as_slice()).expect("changed").console.expect("record").revision, 2);
    let stale = peer.request(v1::ConsolesId::ConsolesRemove, v1::ConsoleRemove { conn: "mm".into(), id: "a".into(), revision: 1 }).await;
    assert_eq!(stale.fault.expect("stale delete").code, "console.changedDelete");
    let removed = peer.request(v1::ConsolesId::ConsolesRemove, v1::ConsoleRemove { conn: "mm".into(), id: "a".into(), revision: 2 }).await;
    assert!(v1::ConsoleRemoved::decode(removed.body.as_slice()).expect("removed").removed);
    let event = peer.event().await;
    assert_eq!((event.id, event.origin.as_str()), (v1::ConsolesId::ConsolesRemoved as u32, "page-a"));
    assert_eq!(v1::ConsoleGone::decode(event.body.as_slice()).expect("gone").id, "a");
    store.put("mm", record("closed", false), SaveMode::Create, "").expect("agent save");
    let event = peer.event().await;
    assert_eq!(event.origin, "");
    assert_eq!(v1::ConsoleSaved::decode(event.body.as_slice()).expect("agent").console.expect("record").id, "closed");
    let cleared = peer.request(v1::ConsolesId::ConsolesClearClosed, v1::ConsolesOf { conn: "mm".into() }).await;
    assert_eq!(v1::ConsolesCleared::decode(cleared.body.as_slice()).expect("cleared").cleared, 1);
    let event = peer.event().await;
    assert_eq!((event.id, event.origin.as_str()), (v1::ConsolesId::ConsolesCleared as u32, "page-a"));
    assert!(peer.list("mm").await.consoles.is_empty());
    let bad = peer.raw(v1::ConsolesId::ConsolesPut, vec![255]).await;
    assert_eq!(bad.fault.expect("bad body").code, swiss_bus::BAD_BODY);
    let unknown = peer.list("mm2").await;
    assert!(unknown.consoles.is_empty());
    // A connection the catalog cannot lease answers its own code: the module is serving.
    let gone = peer.request(v1::ConsolesId::ConsolesPut, v1::ConsolePut { conn: "gone".into(), ..put(record("b", true)) }).await;
    assert_eq!(gone.fault.expect("unleased put").code, "console.connection");
    let listed = peer.request(v1::ConsolesId::ConsolesList, v1::ConsolesPage { conn: "gone".into(), after: String::new() }).await;
    assert_eq!(listed.fault.expect("unleased list").code, "console.connection");
    assert!(store.list("gone").expect("nothing filed").is_empty());
    std::fs::remove_dir_all(root).expect("scratch cleanup");
}

#[tokio::test]
async fn concurrent_console_commits_leave_events_in_revision_order_under_the_write_lock() {
    let root = std::env::temp_dir().join(format!("swiss-bus-order-{}", swiss_core::util::random_hex(8)));
    let bus = Bus::new(Arc::new(String::new));
    let store = ConsoleStore::at(root.clone(), bus.clone());
    store.save("mm", record("same", true)).expect("initial");
    let mut peer = Peer::open(bus).await;
    peer.subscribe("mm").await;
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let writers: Vec<_> = (0..2).map(|writer| {
        let store = store.clone(); let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            for step in 0..4 {
                loop {
                    let mut record = store.list("mm").expect("read").remove(0);
                    record.sql = format!("writer {writer} step {step}");
                    match store.put("mm", record, SaveMode::Update, "") {
                        Ok(_) => break,
                        Err(e) => assert_eq!(e, CHANGED),
                    }
                }
            }
        })
    }).collect();
    for writer in writers { writer.join().expect("writer"); }
    for revision in 2..=9 {
        let event = peer.event().await;
        assert_eq!(v1::ConsoleSaved::decode(event.body.as_slice()).expect("saved").console.expect("record").revision, revision);
    }
    std::fs::remove_dir_all(root).expect("scratch cleanup");
}
