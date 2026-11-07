/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The survey's tests: the skeleton, the manifests and relations, the draft, the prompt and
//! its answer, the review and the hubs (SPEC §memory.map).

use super::*;

fn listed(paths: &[&str]) -> Vec<Listed> {
    paths
        .iter()
        .map(|p| Listed { path: p.to_string(), lang: crate::code::extract::Lang::of(p).map(|l| l.as_str().to_string()), read: true, size: 2048 })
        .collect()
}

fn manifest(dir: &str, file: &str) -> Manifest {
    Manifest { dir: dir.into(), file: file.into(), name: None, description: None, ids: Vec::new(), deps: None }
}

fn texts(files: &[(&str, &str)]) -> HashMap<String, String> {
    files.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect()
}

fn survey(paths: &[&str], files: &[(&str, &str)], defs: Vec<Def>, uses: Vec<(usize, usize, String)>, existing: &[Domain]) -> Survey {
    let text = texts(files);
    let indexed = Indexed { files: listed(paths), defs, uses, imports: Vec::new(), truncated: false };
    let read = |rel: &str, _max: usize| text.get(rel).cloned();
    Survey::build("shop", &indexed, existing, &read, &Draw::default())
}

/// A draft of bare paths, drawn with `draw`.
fn drawn(paths: &[&str], existing: &[Domain], draw: &Draw) -> Survey {
    let indexed = Indexed { files: listed(paths), defs: Vec::new(), uses: Vec::new(), imports: Vec::new(), truncated: false };
    Survey::build("shop", &indexed, existing, &|_: &str, _: usize| None, draw)
}

fn domain(path: &str, owns: &str) -> Domain {
    Domain {
        id: format!("d-{path}"),
        scope: "p".into(),
        path: path.into(),
        title: path.into(),
        summary: String::new(),
        body: String::new(),
        owns: vec![owns.into()],
        origin: crate::model::Origin::User,
        actor: "t".into(),
        confirmed: false,
        revision: 1,
        created_ms: 0,
        updated_ms: 0,
        verified_ms: 0,
        keywords: Vec::new(),
    }
}

#[test]
fn file_names_split_into_words_and_stems() {
    assert_eq!(words("OrderServiceImpl.java"), ["Order", "Service", "Impl"]);
    assert_eq!(words("HTTPServer.go"), ["HTTP", "Server"]);
    assert_eq!(words("memory_api.rs"), ["memory", "api"]);
    assert_eq!(words("order.controller.ts"), ["order"]);
    let stem = |name: &str, prefix: Option<&str>| stem_of(name, prefix).map(|s| (s.key, s.word, s.lead));
    assert_eq!(stem("OrdersController.java", None), Some(("order".into(), "Orders".into(), "Orders".into())));
    assert_eq!(stem("address_book.py", None).map(|s| s.0), Some("address".into()));
    assert_eq!(stem("index.ts", None), None, "a generic name names no business");
    assert_eq!(stem("AbstractThing.java", None), None);
    // An interface's `I` and the project's shared prefix are no business; the lead keeps them.
    assert_eq!(stem("IOrderService.java", None), Some(("order".into(), "Order".into(), "IOrder".into())));
    assert_eq!(stem("ISysUserService.java", Some("sys")), Some(("user".into(), "User".into(), "ISysUser".into())));
    assert_eq!(stem("SysUser.java", Some("sys")), Some(("user".into(), "User".into(), "SysUser".into())));
    assert_eq!(stem("Sys.java", Some("sys")).map(|s| s.0), Some("sys".into()), "a name of the prefix alone keeps it");
    assert_eq!(stem("Item.java", None).map(|s| s.0), Some("item".into()), "an `I` starting a word is no interface marker");
    // A word of one or two letters is an abbreviation: it takes the words after it, up
    // to a generic or a layer's word, before which two letters stand alone.
    let word = |name: &str| stem_of(name, None).map(|s| (s.word, s.lead));
    assert_eq!(word("EcPayChatService.java"), Some(("EcPay".into(), "EcPay".into())));
    assert_eq!(word("FxRateMapper.java"), Some(("FxRate".into(), "FxRate".into())));
    assert_eq!(word("PmService.java"), Some(("Pm".into(), "Pm".into())));
    assert_eq!(word("PmCtl.java"), Some(("Pm".into(), "Pm".into())), "a role's word ends it as a layer's does");
    assert_eq!(word("Pm.java"), None, "two letters alone name nothing");
    assert_eq!(word("PO.java"), None);
    assert_eq!(slug("swiss-Memory v2"), "swiss-memory-v2");
    assert_eq!(slug("@scope/pkg"), "scope-pkg");
    assert_eq!(slug("***"), "area");
}

#[test]
fn tests_vendored_and_docs_are_no_ones_code() {
    let files = listed(&["src/a.rs", "tests/a.rs", "src/a_test.go", "web/node_modules/x/i.js", "docs/x.py", ".github/x.sh", "src/b.test.ts", "db/schema.sql"]);
    let code: Vec<&str> = files.iter().filter(|f| is_code(f)).map(|f| f.path.as_str()).collect();
    assert_eq!(code, ["src/a.rs", "db/schema.sql"]);
}

#[test]
fn a_layered_layout_is_cut_by_business_not_by_layer() {
    let base = "src/main/java/com/acme/shop";
    let paths: Vec<String> = [
        "controller/OrderController.java",
        "controller/PaymentController.java",
        "service/OrderService.java",
        "service/OrderServiceImpl.java",
        "service/PaymentService.java",
        "repository/OrderRepository.java",
        "repository/PaymentRepository.java",
        "model/Order.java",
        "model/Payment.java",
        "config/WebConfig.java",
        "config/DbConfig.java",
        "config/SecurityConfig.java",
        "ShopApplication.java",
    ]
    .iter()
    .map(|p| format!("{base}/{p}"))
    .collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let s = survey(&refs, &[], Vec::new(), Vec::new(), &[]);
    let plan: Vec<(String, Vec<String>, usize)> = s.plan().into_iter().map(|u| (u.path, u.owns, u.files)).collect();
    assert_eq!(
        plan,
        [
            ("order".to_string(), vec![format!("{base}/**/Order*")], 5),
            ("payment".to_string(), vec![format!("{base}/**/Payment*")], 4),
            ("config".to_string(), vec![format!("{base}/config")], 3),
        ]
    );
}

#[test]
fn a_java_project_is_cut_by_business_under_its_package_path_and_its_sql_is_an_area_beside() {
    // A Maven project: the code sits under `src/main/java/<package path>`, the schemas in
    // `sql/` beside `src`. The top directories (`src`, `sql`) are no business.
    let base = "src/main/java/com/acme/shop";
    let mut paths: Vec<String> = vec!["pom.xml".into(), "sql/schema.sql".into(), "sql/data.sql".into(), "sql/upgrade_v2.sql".into()];
    paths.extend(
        [
            "controller/OrderController.java",
            "controller/PaymentController.java",
            "service/OrderService.java",
            "service/OrderServiceImpl.java",
            "service/PaymentService.java",
            "repository/OrderRepository.java",
            "repository/PaymentRepository.java",
            "model/Order.java",
            "model/Payment.java",
            "config/WebConfig.java",
            "config/DbConfig.java",
            "config/SecurityConfig.java",
            "ShopApplication.java",
        ]
        .iter()
        .map(|p| format!("{base}/{p}")),
    );
    paths.push("src/test/java/com/acme/shop/OrderServiceTest.java".into());
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let s = survey(&refs, &[("pom.xml", "<project><artifactId>shop</artifactId></project>")], Vec::new(), Vec::new(), &[]);
    let plan: Vec<(String, Vec<String>, usize)> = s.plan().into_iter().map(|u| (u.path, u.owns, u.files)).collect();
    assert_eq!(
        plan,
        [
            ("order".to_string(), vec![format!("{base}/**/Order*")], 5),
            ("payment".to_string(), vec![format!("{base}/**/Payment*")], 4),
            ("config".to_string(), vec![format!("{base}/config")], 3),
            ("sql".to_string(), vec!["sql".to_string()], 3),
        ]
    );

    // RuoYi-style naming: every class carries the `Sys` prefix and the services are
    // interfaces (`ISysUserService`): the business is the word after both.
    let base = "src/main/java/com/acme/admin";
    let mut paths: Vec<String> = vec!["pom.xml".into(), "sql/ry.sql".into(), "sql/quartz.sql".into(), "sql/upgrade.sql".into()];
    for e in ["User", "Role", "Menu", "Dept"] {
        for p in [
            format!("controller/Sys{e}Controller.java"),
            format!("service/ISys{e}Service.java"),
            format!("service/impl/Sys{e}ServiceImpl.java"),
            format!("mapper/Sys{e}Mapper.java"),
            format!("domain/Sys{e}.java"),
        ] {
            paths.push(format!("{base}/{p}"));
        }
    }
    for p in ["utils/StringUtils.java", "utils/DateUtils.java", "utils/ExcelUtil.java"] {
        paths.push(format!("{base}/{p}"));
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let s = survey(&refs, &[], Vec::new(), Vec::new(), &[]);
    let plan: Vec<(String, Vec<String>, usize)> = s.plan().into_iter().map(|u| (u.path, u.owns, u.files)).collect();
    let stem = |name: &str| (name.to_ascii_lowercase(), vec![format!("{base}/**/ISys{name}*"), format!("{base}/**/Sys{name}*")], 5);
    assert_eq!(
        plan,
        [
            stem("Dept"),
            stem("Menu"),
            stem("Role"),
            stem("User"),
            ("utils".to_string(), vec![format!("{base}/utils")], 3),
            ("sql".to_string(), vec!["sql".to_string()], 3),
        ]
    );

    // A business word starting most names is no prefix: `Order` goes on with layers.
    let names: Vec<String> = ["OrderController", "OrderService", "OrderServiceImpl", "OrderItem", "OrderLine", "OrderStatus", "PaymentController"]
        .iter()
        .map(|n| format!("x/{n}.java"))
        .collect();
    let all: Vec<usize> = (0..names.len()).collect();
    assert_eq!(shared_prefix(&names, &all), None);
    let hooks: Vec<String> = ["useOrders", "useCart", "usePayment", "useUser", "useSession", "useTheme"].iter().map(|n| format!("x/{n}.ts")).collect();
    assert_eq!(shared_prefix(&hooks, &all[..6]).as_deref(), Some("use"));
}

#[test]
fn a_layered_java_project_is_cut_by_business_across_web_service_and_dal() {
    // The shape of a billing backend: `web`, `service` and `dal` (its mappers and
    // POs beneath) are the layers, `core` and `util` technical directories, and a script
    // at the checkout's root makes the split's base the root.
    let base = "src/main/java/com/acme/billing";
    let mut paths: Vec<String> = vec!["fix-script.js".into(), "script/a.sql".into(), "script/b.sql".into(), "script/c.sql".into()];
    for p in [
        "web/InvoiceCtl.java",
        "web/v4/InvoiceCtlV4.java",
        "web/wholesale/InvoiceWholesaleCtl.java",
        "web/InvoiceItemCtl.java",
        "web/res/invoice/SummaryRes.java",
        "web/res/invoice/DetailRes.java",
        "service/InvoiceService.java",
        "service/InvoiceLineService.java",
        "dal/mapper/InvoiceMapper.java",
        "dal/mapper/InvoiceLineMapper.java",
        "dal/mapper/InvoiceItemMapper.java",
        "dal/po/InvoiceLinePO.java",
        "dal/po/InvoiceItemPO.java",
        "web/OrderCtl.java",
        "service/OrderSyncService.java",
        "dal/mapper/OrderMapper.java",
        "web/dto/orders/BuyerOrderStat.java",
        "web/dto/orders/SellerStat.java",
        "web/v4/EcPayCtlV4.java",
        "service/ecpay/EcPayChatService.java",
        "service/ecpay/EcPayQuotaService.java",
        "dal/mapper/EcPayMessageMapper.java",
        "web/PmCtl.java",
        "service/PmService.java",
        "core/enums/OrderStatusEnum.java",
        "core/enums/SideEnum.java",
        "core/constants/Keys.java",
        "util/DateUtil.java",
        "util/JsonUtil.java",
        "util/IpUtil.java",
        "Bootstrap.java",
    ] {
        paths.push(format!("{base}/{p}"));
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let s = drawn(&refs, &[], &Draw::default());
    let plan: Vec<(String, Vec<String>, usize)> = s.plan().into_iter().map(|u| (u.path, u.owns, u.files)).collect();
    let glob = |lead: &str| format!("{base}/**/{lead}*");
    assert_eq!(
        plan,
        [
            // A stem area's parts are its longer stems, never a layer's whole directory;
            // a directory in a layer named for the business is the business's.
            ("invoice".to_string(), vec![glob("Invoice"), format!("{base}/web/res/invoice")], 7),
            ("invoice/item".to_string(), vec![glob("InvoiceItem")], 3),
            ("invoice/line".to_string(), vec![glob("InvoiceLine")], 3),
            ("order".to_string(), vec![glob("Order"), format!("{base}/web/dto/orders")], 6),
            // `EcPay` and `Pm` are abbreviations, no names too short to be a business.
            ("ecpay".to_string(), vec![glob("EcPay"), format!("{base}/service/ecpay")], 4),
            ("core".to_string(), vec![format!("{base}/core")], 2),
            ("util".to_string(), vec![format!("{base}/util")], 3),
            ("pm".to_string(), vec![glob("Pm")], 2),
            ("script".to_string(), vec!["script".to_string()], 3),
        ]
    );
    assert_eq!((s.code_files, s.owned_files), (35, 33), "the root script and the entry class are no area's");
}

/// A layered project whose `Invoice` business goes four stems deep: `Invoice`,
/// `InvoiceLine`, `InvoiceLineHistory` and the history's `Daily`, `Weekly` and
/// `Monthly` - each name a service, a mapper and a model - beside a three-file `Team`.
fn deep_invoice() -> (String, Vec<String>) {
    let base = "src/main/java/com/acme/billing".to_string();
    let mut paths: Vec<String> = Vec::new();
    for name in [
        "Invoice",
        "InvoiceFee",
        "InvoiceLine",
        "InvoiceLineSnapshot",
        "InvoiceLineHistory",
        "InvoiceLineHistoryDaily",
        "InvoiceLineHistoryWeekly",
        "InvoiceLineHistoryMonthly",
        "Team",
    ] {
        paths.push(format!("{base}/service/{name}Service.java"));
        paths.push(format!("{base}/mapper/{name}Mapper.java"));
        paths.push(format!("{base}/model/{name}.java"));
    }
    (base, paths)
}

#[test]
fn a_large_business_is_split_largest_first_down_to_its_fourth_level() {
    let (base, paths) = deep_invoice();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let s = drawn(&refs, &[], &Draw::default());
    let plan: Vec<(String, Vec<String>, usize)> = s.plan().into_iter().map(|u| (u.path, u.owns, u.files)).collect();
    let glob = |lead: &str| vec![format!("{base}/**/{lead}*")];
    assert_eq!(
        plan,
        [
            // 24 files: a part of 12 or more splits again, a part of three stays whole.
            ("invoice".to_string(), glob("Invoice"), 3),
            ("invoice/line".to_string(), glob("InvoiceLine"), 3),
            ("invoice/line/history".to_string(), glob("InvoiceLineHistory"), 3),
            ("invoice/line/history/daily".to_string(), glob("InvoiceLineHistoryDaily"), 3),
            ("invoice/line/history/monthly".to_string(), glob("InvoiceLineHistoryMonthly"), 3),
            ("invoice/line/history/weekly".to_string(), glob("InvoiceLineHistoryWeekly"), 3),
            ("invoice/line/snapshot".to_string(), glob("InvoiceLineSnapshot"), 3),
            ("invoice/fee".to_string(), glob("InvoiceFee"), 3),
            ("team".to_string(), glob("Team"), 3),
        ]
    );
    let sizes: Vec<usize> = s.levels().iter().map(Vec::len).collect();
    assert_eq!(sizes, [3, 2, 2, 2], "the deepest level is asked first");

    // With room for few more areas, the largest split first and the last split that
    // fits keeps its largest parts: a part left out stays with its parent.
    let all: Vec<usize> = (0..paths.len()).collect();
    let drawn_with = |room: usize| -> Vec<String> {
        let mut top = split(&paths, &all);
        split_largest_first(&paths, &mut top, room);
        let mut out = Vec::new();
        let mut stack: Vec<(&Group, String)> = top.iter().rev().map(|g| (g, slug(&g.name))).collect();
        while let Some((group, path)) = stack.pop() {
            stack.extend(group.children.iter().rev().map(|c| (c, format!("{path}/{}", slug(&c.name)))));
            out.push(path);
        }
        out
    };
    assert_eq!(drawn_with(3), ["invoice", "invoice/line", "invoice/fee", "team"], "no room for a second split");
    assert_eq!(
        drawn_with(6),
        [
            "invoice",
            "invoice/line",
            "invoice/line/history",
            "invoice/line/history/daily",
            "invoice/line/history/monthly",
            "invoice/line/snapshot",
            "invoice/fee",
            "team",
        ]
    );

    // A review's parent over the map takes the fifth level, the deepest a map goes.
    let review = s
        .check_review(&json!({"domains": [{"path": "billing", "title": "Billing", "members": ["invoice", "team"], "merge": false}]}))
        .expect("an answer");
    let s = drawn(&refs, &[], &Draw { regroup: Some(review), ignore: Vec::new() });
    let fifth: Vec<&str> = s.units.iter().map(|u| u.path.as_str()).filter(|p| p.split('/').count() == DOMAIN_DEPTH_MAX).collect();
    let history = "billing/invoice/line/history";
    assert_eq!(fifth, [format!("{history}/daily"), format!("{history}/monthly"), format!("{history}/weekly")]);
    assert_eq!(crate::model::check_domain_path(fifth[0]).as_deref(), Ok(fifth[0]));
    assert_eq!(s.levels().len(), DOMAIN_DEPTH_MAX);
    assert!(crate::model::check_domain_path("billing/invoice/line/history/daily/x").is_err(), "six levels are too many");
}

#[test]
fn a_business_ranks_by_the_files_it_owns_its_directories_too() {
    // Fifty businesses of three classes each, and `Pm` of two whose message components
    // sit in `dto/pm`: past the cap of businesses, `Pm` ranks by the eleven files it owns,
    // not the two its name starts - a layered backend lost a whole business this way.
    let base = "src/main/java/com/acme/big";
    let mut paths: Vec<String> = Vec::new();
    for i in 0..50u8 {
        let w = format!("Z{}{}", (b'a' + i / 26) as char, (b'a' + i % 26) as char);
        paths.push(format!("{base}/controller/{w}Controller.java"));
        paths.push(format!("{base}/service/{w}Service.java"));
        paths.push(format!("{base}/service/{w}ServiceImpl.java"));
    }
    paths.push(format!("{base}/controller/PmController.java"));
    paths.push(format!("{base}/service/PmService.java"));
    for name in ["Text", "Image", "Audio", "Video", "Button", "Link", "Coupon", "Spacer", "Row"] {
        paths.push(format!("{base}/dto/pm/{name}Component.java"));
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let s = drawn(&refs, &[], &Draw::default());
    let plan = s.plan();
    let pm = plan.iter().find(|u| u.path == "pm").unwrap_or_else(|| panic!("{:?}", plan.iter().map(|u| &u.path).collect::<Vec<_>>()));
    assert_eq!((pm.owns.clone(), pm.files), (vec![format!("{base}/**/Pm*"), format!("{base}/dto/pm")], 11));
    assert_eq!(s.units.iter().filter(|u| u.kind == UnitKind::Stem).count(), STEMS_MAX, "the smallest businesses go");
    assert!(!plan.iter().any(|u| u.path == "zbx"), "the last of the three-file businesses is past the cap");
}

#[test]
fn every_business_stem_reaches_the_review_and_a_merge_past_a_domains_entries_nests() {
    // Thirty-two businesses, each a controller and a service: more than the fourteen
    // directory areas a top level holds, and all of them what the review groups.
    let base = "src/main/java/com/acme/big";
    let words: Vec<String> = (0..32u8).map(|i| format!("Z{}{}", (b'a' + i / 26) as char, (b'a' + i % 26) as char)).collect();
    let mut paths: Vec<String> = Vec::new();
    for w in &words {
        paths.push(format!("{base}/controller/{w}Controller.java"));
        paths.push(format!("{base}/service/{w}Service.java"));
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let skeleton = drawn(&refs, &[], &Draw::default());
    assert_eq!(skeleton.units.len(), 32, "{:?}", skeleton.plan().iter().map(|u| u.path.clone()).collect::<Vec<_>>());
    let prompt = skeleton.review_prompt(None).expect("reviewed");
    assert!(prompt.contains("### `zaa`") && prompt.contains("### `zbf`"), "{prompt}");

    // Merged, the thirty-two entries pass what one domain owns: the members nest instead.
    let members: Vec<String> = words.iter().map(|w| w.to_ascii_lowercase()).collect();
    let review = skeleton.check_review(&json!({"domains": [{"path": "all", "title": "All", "members": members, "merge": true}]})).expect("an answer");
    let s = drawn(&refs, &[], &Draw { regroup: Some(review), ignore: Vec::new() });
    assert_eq!((s.units[0].path.as_str(), s.units[0].kind, s.units[0].owns.len()), ("all", UnitKind::Group, 0));
    assert_eq!(s.units[0].children.len(), 32);
    assert_eq!(s.owned_files, 64, "no file is lost to a cut list of entries");

    // Fourteen technical directories, fifty businesses and the database scripts beside are
    // more than a top level holds: the smallest businesses go, the rest stay whole.
    for i in 32..50u8 {
        let w = format!("Z{}{}", (b'a' + i / 26) as char, (b'a' + i % 26) as char);
        paths.push(format!("{base}/controller/{w}Controller.java"));
        paths.push(format!("{base}/service/{w}Service.java"));
    }
    for d in ["aa", "bb", "cc", "dd", "ee", "ff", "gg", "hh", "ii", "jj", "kk", "ll", "mm", "nn"] {
        for f in ["A", "B", "C", "D"] {
            paths.push(format!("{base}/tech{d}/{f}.java"));
        }
    }
    paths.extend(["sql/a.sql", "sql/b.sql", "sql/c.sql"].map(String::from));
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let s = drawn(&refs, &[], &Draw::default());
    let stems = s.units.iter().filter(|u| u.kind == UnitKind::Stem).count();
    assert_eq!((s.units.len(), stems), (STEMS_MAX + 15, STEMS_MAX), "{:?}", s.plan().iter().map(|u| u.path.clone()).collect::<Vec<_>>());
    assert!(s.units.iter().any(|u| u.path == "sql") && s.units.iter().any(|u| u.path == "zbf"), "the scripts and every two-file stem stay");
}

#[test]
fn a_workspace_is_cut_by_package_and_the_root_package_keeps_its_sources() {
    let mut paths = vec!["Cargo.toml", "crates/shop-core/Cargo.toml", "crates/shop-web/Cargo.toml", "README.md"];
    let core = ["crates/shop-core/src/lib.rs", "crates/shop-core/src/money.rs", "crates/shop-core/src/tax.rs"];
    let web = ["crates/shop-web/src/lib.rs", "crates/shop-web/src/routes.rs", "crates/shop-web/src/views.rs"];
    let app = ["src/main.rs", "src/cli.rs", "src/serve.rs", "scripts/deploy.sh"];
    paths.extend(core);
    paths.extend(web);
    paths.extend(app);
    let files = [
        ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n\n[package]\nname = \"shop\"\ndescription = \"The shop\"\n"),
        ("crates/shop-core/Cargo.toml", "[package]\nname = \"shop-core\"\ndescription = \"Money and tax\"\n"),
        ("crates/shop-web/Cargo.toml", "[package]\nname = \"shop-web\"\n\n[dependencies]\nshop-core = { path = \"../shop-core\" }\n"),
        ("README.md", "# Shop\nSells things.\n"),
        ("src/main.rs", "//! The shop's binary.\nfn main() {}\n"),
    ];
    // The web package uses the core's `MoneyAmount` (defined in money.rs); the core spells
    // a name the web defines too, but its manifest depends on no web: no relation.
    let money = paths.iter().position(|p| *p == "crates/shop-core/src/money.rs").unwrap();
    let routes = paths.iter().position(|p| *p == "crates/shop-web/src/routes.rs").unwrap();
    let tax = paths.iter().position(|p| *p == "crates/shop-core/src/tax.rs").unwrap();
    let views = paths.iter().position(|p| *p == "crates/shop-web/src/views.rs").unwrap();
    let defs = vec![
        Def { file: money, name: "MoneyAmount".into(), kind: "struct", line: 3, fanin: 1 },
        Def { file: views, name: "render_page".into(), kind: "fn", line: 3, fanin: 1 },
    ];
    let uses = vec![(routes, money, "MoneyAmount".to_string()), (tax, views, "render_page".to_string())];
    let s = survey(&paths, &files, defs, uses, &[]);
    let plan: Vec<(String, Vec<String>)> = s.plan().into_iter().map(|u| (u.path, u.owns)).collect();
    assert_eq!(
        plan,
        [
            ("core".to_string(), vec!["crates/shop-core".to_string()]),
            ("shop".to_string(), vec!["src".to_string()]),
            ("web".to_string(), vec!["crates/shop-web".to_string()]),
        ]
    );
    let web = s.units.iter().position(|u| u.path == "web").unwrap();
    let core = s.units.iter().position(|u| u.path == "core").unwrap();
    assert_eq!(s.units[web].uses, [Relation { unit: core, weight: 1, via: vec!["MoneyAmount".into()] }]);
    assert!(s.units[core].uses.is_empty(), "a name the core's manifest cannot reach: {:?}", s.units[core].uses);
    assert_eq!(s.units[core].manifest.as_ref().and_then(|m| m.description.as_deref()), Some("Money and tax"));
    let shop = s.units.iter().position(|u| u.path == "shop").unwrap();
    assert_eq!(s.units[shop].entries, ["src/main.rs"]);
    assert!(s.units[shop].excerpts[0].2.contains("The shop's binary"), "the entry file's head is read");
    let prompt = s.prompt(web, &vec![None; s.units.len()], Some("zh"));
    assert!(prompt.starts_with(MARKER));
    assert!(prompt.contains("- Uses `core`"), "{prompt}");
    assert!(prompt.contains("Sells things."), "a top-level area reads the project's README");
    assert!(prompt.contains("in English and in Simplified Chinese"), "{prompt}");
}

#[test]
fn a_flat_directory_is_cut_by_name_stem_and_a_module_file_joins_its_directory() {
    let paths = [
        "src/plugins/memory.rs",
        "src/plugins/memory_api.rs",
        "src/plugins/memory_mcp.rs",
        "src/plugins/memory_sync.rs",
        "src/plugins/memory_render.rs",
        "src/plugins/llm.rs",
        "src/plugins/llm_api.rs",
        "src/plugins/llm_agents.rs",
        "src/plugins/llm_otel.rs",
        "src/plugins/redis.rs",
        "src/plugins/redis_pool.rs",
        "src/plugins/redis_cmd.rs",
        "src/plugins/mod.rs",
        "src/plugins/auth.rs",
        "src/plugins/cors.rs",
        "src/store.rs",
        "src/store/sqlite.rs",
        "src/store/schema.rs",
        "src/store/migrate.rs",
        "src/main.rs",
    ];
    let s = survey(&paths, &[], Vec::new(), Vec::new(), &[]);
    let plan: Vec<(String, Vec<String>, usize)> = s.plan().into_iter().map(|u| (u.path, u.owns, u.files)).collect();
    assert_eq!(
        plan,
        [
            ("plugins".to_string(), vec!["src/plugins".to_string()], 3),
            ("plugins/memory".to_string(), vec!["src/plugins/memory*".to_string()], 5),
            ("plugins/llm".to_string(), vec!["src/plugins/llm*".to_string()], 4),
            // The plural fold joins `job` and `jobs`; the name stays the files' own.
            ("plugins/redis".to_string(), vec!["src/plugins/redis*".to_string()], 3),
            ("store".to_string(), vec!["src/store".to_string(), "src/store.rs".to_string()], 4),
        ]
    );
}

#[test]
fn a_review_regroups_the_top_level_into_business_domains_and_its_mistakes_are_dropped() {
    let base = "src/main/java/com/acme/shop";
    let mut paths: Vec<String> = vec!["sql/schema.sql".into(), "sql/data.sql".into(), "sql/upgrade_v2.sql".into()];
    for p in [
        "controller/OrderController.java",
        "controller/PaymentController.java",
        "service/OrderService.java",
        "service/OrderServiceImpl.java",
        "service/PaymentService.java",
        "repository/OrderRepository.java",
        "repository/PaymentRepository.java",
        "model/Order.java",
        "model/Payment.java",
        "config/WebConfig.java",
        "config/DbConfig.java",
        "config/SecurityConfig.java",
    ] {
        paths.push(format!("{base}/{p}"));
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let skeleton = drawn(&refs, &[], &Draw::default());
    let tops: Vec<&str> = skeleton.units.iter().map(|u| u.path.as_str()).collect();
    assert_eq!(tops, ["order", "payment", "config", "sql"]);
    let prompt = skeleton.review_prompt(Some("zh")).expect("four new areas are reviewed");
    for said in ["### `order`", "### `sql`", "Files: OrderController.java", "Write each title in Simplified Chinese"] {
        assert!(prompt.contains(said), "{said} missing from:\n{prompt}");
    }

    let answer = json!({"domains": [
        {"path": "commerce", "title": "交易", "members": ["order", "payment"], "merge": false},
        {"path": "platform", "title": "平台", "members": ["config", "billing"], "merge": false},
        {"path": "order", "title": "x", "members": ["order"], "merge": false},
        {"path": "Data/Base", "title": "x", "members": ["sql"], "merge": false},
        {"path": "sql", "title": "", "members": ["order", "payment"], "merge": true},
    ]});
    let regroup = skeleton.check_review(&answer).expect("an object with a domains list");
    let kept: Vec<(&str, &str, Vec<&str>)> =
        regroup.domains.iter().map(|d| (d.path.as_str(), d.title.as_str(), d.members.iter().map(String::as_str).collect())).collect();
    assert_eq!(kept, [("commerce", "交易", vec!["order", "payment"]), ("platform", "平台", vec!["config"])]);
    assert_eq!(regroup.dropped.len(), 7, "{:?}", regroup.dropped);
    assert!(regroup.dropped.iter().any(|d| d.contains("`billing`")), "{:?}", regroup.dropped);
    assert!(skeleton.check_review(&json!(["order"])).is_err(), "no object is no answer");

    // Drawn again with the review: a parent owning nothing over its members, a rename.
    let s = drawn(&refs, &[], &Draw { regroup: Some(regroup), ignore: Vec::new() });
    let plan: Vec<(String, UnitKind, usize)> = s.plan().into_iter().map(|u| (u.path, u.kind, u.files)).collect();
    assert_eq!(
        plan,
        [
            ("commerce".to_string(), UnitKind::Group, 0),
            ("commerce/order".to_string(), UnitKind::Stem, 5),
            ("commerce/payment".to_string(), UnitKind::Stem, 4),
            ("platform".to_string(), UnitKind::Dir, 3),
            ("sql".to_string(), UnitKind::Dir, 3),
        ]
    );
    assert_eq!(s.units[0].title, "交易");
    assert!(s.units[0].owns.is_empty());
    assert_eq!(s.levels(), [vec![1, 2], vec![0, 3, 4]], "sub-areas are asked before their parent");
    let hub = s.hub(0, None);
    assert!(hub.contains("The business domain over `commerce/order`, `commerce/payment`."), "{hub}");
    assert!(s.prompt(0, &[], None).contains("it owns no code itself"));

    // A merge makes one area of its members, owning what each owned.
    let merge = skeleton
        .check_review(&json!({"domains": [{"path": "order", "title": "Orders", "members": ["order", "payment"], "merge": true}]}))
        .expect("an answer");
    let s = drawn(&refs, &[], &Draw { regroup: Some(merge), ignore: Vec::new() });
    let top = &s.units[0];
    assert_eq!((top.path.as_str(), top.kind, top.files.len()), ("order", UnitKind::Group, 9));
    assert_eq!(top.owns, [format!("{base}/**/Order*"), format!("{base}/**/Payment*")]);

    // A path the map has is taken - unless a redraw sets that domain aside.
    let map = [domain("commerce", "elsewhere")];
    let on_map = drawn(&refs, &map, &Draw::default());
    let refused = on_map.check_review(&json!({"domains": [{"path": "commerce", "title": "C", "members": ["order", "payment"], "merge": false}]})).expect("an answer");
    assert!(refused.domains.is_empty() && refused.dropped[0].contains("the map or another entry has it"), "{refused:?}");
    let aside = drawn(&refs, &map, &Draw { regroup: None, ignore: vec!["commerce".into()] });
    let taken = aside.check_review(&json!({"domains": [{"path": "commerce", "title": "C", "members": ["order", "payment"], "merge": false}]})).expect("an answer");
    assert_eq!(taken.domains.len(), 1, "{taken:?}");

    // Two new areas read as what they are: no review.
    let small = drawn(&["web/a.ts", "web/b.ts", "web/c.ts", "api/a.go", "api/b.go", "api/c.go"], &[], &Draw::default());
    assert!(small.review_prompt(None).is_none());
}

#[test]
fn what_the_map_has_is_kept_and_what_it_owns_is_left_out() {
    let paths = ["web/a.ts", "web/b.ts", "web/c.ts", "api/a.go", "api/b.go", "api/c.go", "jobs/a.py", "jobs/b.py", "jobs/c.py"];
    let s = survey(&paths, &[], Vec::new(), Vec::new(), &[domain("frontend", "web"), domain("api", "elsewhere")]);
    let plan: Vec<(String, bool)> = s.plan().into_iter().map(|u| (u.path, u.kept)).collect();
    assert_eq!(plan, [("jobs".to_string(), false)], "web is owned already; api is on the map and has no new child");
    assert_eq!(s.levels(), [vec![0]]);
    // A map that owns everything leaves nothing to draw, and says so.
    let s = survey(&paths, &[], Vec::new(), Vec::new(), &[domain("frontend", "web"), domain("api", "api"), domain("jobs", "jobs")]);
    assert!(s.plan().is_empty() && s.mapped_files == 9);
    assert!(s.outline().contains("(nothing new to draw: the map already owns 9 of the code files)"), "{}", s.outline());
}

#[test]
fn an_answer_is_checked_against_the_index() {
    let paths = ["svc/orders/place.go", "svc/orders/cancel.go", "svc/orders/store.go", "svc/billing/charge.go", "svc/billing/refund.go", "svc/billing/ledger.go"];
    let defs = vec![
        Def { file: 0, name: "PlaceOrder".into(), kind: "fn", line: 4, fanin: 2 },
        Def { file: 3, name: "ChargeCard".into(), kind: "fn", line: 9, fanin: 1 },
    ];
    let uses = vec![(0, 3, "ChargeCard".to_string())];
    let s = survey(&paths, &[], defs, uses, &[]);
    let orders = s.units.iter().position(|u| u.path == "orders").unwrap();
    let answer = json!({
        "title": "Orders",
        "summary": "Takes and cancels customer orders.",
        "overview": "The order lifecycle.",
        "concepts": [{"name": "Order", "meaning": "a customer's purchase"}, "Hold: stock kept for an order"],
        "entryPoints": [{"name": "PlaceOrder", "role": "takes an order"}, {"name": "MakeCoffee", "role": "invented"}],
        "flow": ["1. `PlaceOrder` checks the cart", "then charges via `ChargeCard`"],
        "code": [{"name": "place.go", "role": "placing"}, {"name": "svc/orders/ghost.go", "role": "invented"}],
        "data": ["table orders"],
        "dependsOn": [{"area": "billing", "why": "charges the card"}, {"area": "nowhere", "why": "x"}],
        "keywords": ["order", "下单, 订单", "order", "a keyword that is far too long to be one search term at all"]
    });
    let fill = s.check(orders, &answer).unwrap();
    assert_eq!(fill.entry_points, [("PlaceOrder".to_string(), "takes an order".to_string())]);
    assert_eq!(fill.code, [("svc/orders/place.go".to_string(), "placing".to_string())]);
    assert_eq!(fill.concepts[1], ("Hold".to_string(), "stock kept for an order".to_string()));
    assert_eq!(fill.flow[0], "`PlaceOrder` checks the cart");
    assert_eq!(fill.keywords, ["order", "下单", "订单"]);
    assert_eq!(fill.dropped, ["MakeCoffee", "svc/orders/ghost.go", "nowhere"]);
    let billing = s.units.iter().position(|u| u.path == "billing").unwrap();
    assert_eq!(fill.depends_on, [(billing, "charges the card".to_string())]);
    assert!(s.check(orders, &json!({"summary": "no title"})).is_err());
    assert!(s.check(orders, &json!("text")).is_err());

    let mut fills = vec![None; s.units.len()];
    fills[orders] = Some(fill);
    let proposal = s.proposal(&fills);
    let op = proposal.domains.iter().find(|d| d.path == "orders").unwrap();
    let body = op.body.as_deref().unwrap();
    assert!(body.contains("## Overview\nThe order lifecycle."), "{body}");
    assert!(body.contains("- `PlaceOrder` (`svc/orders/place.go`) - takes an order"), "{body}");
    assert!(body.contains("- Uses `billing` - charges the card"), "{body}");
    assert_eq!(op.keywords.as_deref(), Some(&["order".to_string(), "下单".into(), "订单".into()][..]));
    // An area with no answer still gets a hub from the index.
    let other = proposal.domains.iter().find(|d| d.path == "billing").unwrap();
    assert_eq!(other.title.as_deref(), Some("Billing"));
    let body = other.body.as_deref().unwrap();
    assert!(body.starts_with("## Overview\n3 code file(s) (go) under svc/billing."), "{body}");
    assert!(body.contains("- Used by `orders` (via `ChargeCard`)"), "{body}");
    assert!(proposal.summary.contains("described 1 of them"));
}

#[test]
fn imports_resolve_to_files_packages_and_modules() {
    let files = listed(&["web/src/app.ts", "web/src/api/index.ts", "core/src/lib.rs", "core/src/store/mod.rs", "go/internal/order/svc.go", "py/shop/orders/service.py"]);
    let manifests = vec![
        Manifest { name: Some("shop-core".into()), ..manifest("core", "Cargo.toml") },
        Manifest { name: Some("example.com/shop".into()), ..manifest("go", "go.mod") },
    ];
    let r = Resolver::new(&files, &manifests);
    assert_eq!(r.resolve("web/src/app.ts", "./api").as_deref(), Some("web/src/api/index.ts"));
    assert_eq!(r.resolve("web/src/app.ts", "../../core/x").as_deref(), None);
    assert_eq!(r.resolve("core/src/lib.rs", "crate::store::Store").as_deref(), Some("core/src/store/mod.rs"));
    assert_eq!(r.resolve("x/main.rs", "shop_core::store").as_deref(), Some("core"));
    assert_eq!(r.resolve("go/cmd/main.go", "example.com/shop/internal/order").as_deref(), Some("go/internal/order"));
    assert_eq!(r.resolve("py/app.py", "shop.orders.service").as_deref(), Some("py/shop/orders/service.py"));
    assert_eq!(r.resolve("py/app.py", "os").as_deref(), None);
}

#[test]
fn an_area_whose_code_sits_one_directory_down_is_split_inside_it() {
    let mut paths = vec!["core/Cargo.toml", "core/src/lib.rs", "core/src/money.rs", "core/src/tax.rs", "panel/Cargo.toml", "panel/src/lib.rs"];
    let ts: Vec<String> = ["views", "ui", "api"]
        .iter()
        .flat_map(|d| ["a", "b", "c", "d"].iter().map(move |f| format!("panel/web/src/{d}/{f}.ts")))
        .collect();
    paths.extend(ts.iter().map(String::as_str));
    let files = [("core/Cargo.toml", "[package]\nname = \"core\"\n"), ("panel/Cargo.toml", "[package]\nname = \"panel\"\n")];
    let s = survey(&paths, &files, Vec::new(), Vec::new(), &[]);
    let plan: Vec<String> = s.plan().into_iter().map(|u| u.path).collect();
    assert_eq!(plan, ["panel", "panel/api", "panel/ui", "panel/views", "core"], "{plan:?}");
}

#[test]
fn manifests_say_what_they_depend_on() {
    let cargo = "[package]\nname = \"shop_web\"\n\n[dependencies]\nshop-core = { path = \"../core\" }\nserde = \"1\"\n\
                 money = { path = \"../package\", package = \"shop-money\" }\nshop-auth.workspace = true\n\n\
                 [target.'cfg(windows)'.dependencies]\nwinapi = \"0.3\"\n\n[dev-dependencies.shop-fixtures]\npath = \"../fx\"\n\n\
                 [build-dependencies.cc2]\npackage = \"shop-build\"\n\n[workspace.dependencies]\nshop-cli = { path = \"cli\" }\n";
    let m = read_manifest("web/Cargo.toml", cargo).unwrap();
    assert_eq!(m.ids, ["shop-web"]);
    assert_eq!(
        m.deps.unwrap(),
        ["shop-core", "serde", "shop-money", "shop-auth", "winapi", "shop-fixtures", "shop-build"],
        "the workspace's own table is no dependency"
    );
    let npm = read_manifest("app/package.json", r#"{"name": "@shop/App", "dependencies": {"@shop/core": "*"}, "devDependencies": {"vitest": "5"}}"#).unwrap();
    assert_eq!((npm.ids, npm.deps.unwrap()), (vec!["@shop/app".to_string()], vec!["@shop/core".to_string(), "vitest".to_string()]));
    let go = read_manifest("svc/go.mod", "module example.com/shop/svc\n\nrequire example.com/shop/core v0.1.0\nrequire (\n\tgithub.com/x/y v1.2.3 // indirect\n)\n").unwrap();
    assert_eq!(go.deps.unwrap(), ["example.com/shop/core", "github.com/x/y"]);
    let pom = "<project><parent><artifactId>shop</artifactId></parent><artifactId>shop-web</artifactId><name>Web</name>\
               <dependencies><dependency><groupId>g</groupId><artifactId>shop-core</artifactId></dependency></dependencies></project>";
    let pom = read_manifest("web/pom.xml", pom).unwrap();
    assert_eq!((pom.name.as_deref(), pom.ids, pom.deps.unwrap()), (Some("Web"), vec!["shop-web".to_string()], vec!["shop-core".to_string()]));
    let py = read_manifest("py/pyproject.toml", "[project]\nname = \"shop\"\n").unwrap();
    assert_eq!(py.deps, None, "a format whose dependencies are not read gates nothing");
}

#[test]
fn a_name_spelled_alike_is_no_relation_across_languages_or_undeclared_packages() {
    let paths = vec![
        "core/Cargo.toml", "core/src/lib.rs", "core/src/store.rs", "core/src/limits.rs",
        "web/package.json", "web/src/app.ts", "web/src/store.ts", "web/src/views.ts",
        "jobs/Cargo.toml", "jobs/src/lib.rs", "jobs/src/queue.rs", "jobs/src/retry.rs",
    ];
    let files = [
        ("core/Cargo.toml", "[package]\nname = \"core\"\n"),
        // The web even declares the core: the language still tells the names apart.
        ("web/package.json", r#"{"name": "web", "dependencies": {"core": "*"}}"#),
        ("jobs/Cargo.toml", "[package]\nname = \"jobs\"\n[dependencies]\ncore = { path = \"../core\" }\n"),
    ];
    let at = |p: &str| paths.iter().position(|x| *x == p).unwrap();
    let defs = vec![
        Def { file: at("core/src/store.rs"), name: "StoreError".into(), kind: "enum", line: 1, fanin: 2 },
        Def { file: at("jobs/src/queue.rs"), name: "enqueue".into(), kind: "fn", line: 1, fanin: 1 },
        Def { file: at("core/src/limits.rs"), name: "FORMAT_VERSION".into(), kind: "const", line: 1, fanin: 0 },
        Def { file: at("jobs/src/retry.rs"), name: "FORMAT_VERSION".into(), kind: "const", line: 1, fanin: 0 },
    ];
    let uses = vec![
        // jobs depends on core: a relation.
        (at("jobs/src/retry.rs"), at("core/src/store.rs"), "StoreError".to_string()),
        // the TypeScript spells the Rust name: a coincidence.
        (at("web/src/store.ts"), at("core/src/store.rs"), "StoreError".to_string()),
        // core depends on no jobs: a coincidence.
        (at("core/src/limits.rs"), at("jobs/src/queue.rs"), "enqueue".to_string()),
        // jobs names its own FORMAT_VERSION: no use of core's.
        (at("jobs/src/lib.rs"), at("core/src/limits.rs"), "FORMAT_VERSION".to_string()),
    ];
    let s = survey(&paths, &files, defs, uses, &[]);
    let unit = |path: &str| s.units.iter().position(|u| u.path == path).unwrap();
    let uses_of = |path: &str| s.units[unit(path)].uses.iter().map(|r| s.units[r.unit].path.clone()).collect::<Vec<_>>();
    assert_eq!(uses_of("jobs"), ["core"]);
    let via = &s.units[unit("jobs")].uses[0].via;
    assert_eq!(via, &["StoreError"], "a name jobs defines itself binds there");
    assert!(uses_of("web").is_empty());
    assert!(uses_of("core").is_empty());
}

#[test]
fn files_a_repository_marks_generated_or_vendored_are_no_area() {
    let rules = marked_rules(
        "* text=auto eol=lf\n# emitted\nui/dist/**/*.js linguist-generated=true\n*.pb.go linguist-generated\n\
         /third_party/** linguist-vendored\nthird_party/ours/** -linguist-vendored\nlib/keep.js linguist-generated=false\n",
    );
    assert!(marked(&rules, "ui/dist/views/a.js"));
    assert!(marked(&rules, "api.pb.go") && marked(&rules, "svc/api/order.pb.go"), "a slashless pattern matches at any depth");
    assert!(marked(&rules, "third_party/zlib/z.c"));
    assert!(!marked(&rules, "third_party/ours/x.c"), "a later rule unsets it");
    assert!(!marked(&rules, "ui/src/views/a.ts") && !marked(&rules, "lib/keep.js"));

    let paths = vec![".gitattributes", "ui/src/a.ts", "ui/src/b.ts", "ui/src/c.ts", "ui/dist/a.js", "ui/dist/b.js", "ui/dist/c.js", "ui/dist/d.js"];
    let files = [(".gitattributes", "ui/dist/** linguist-generated\n")];
    let s = survey(&paths, &files, Vec::new(), Vec::new(), &[]);
    let owned: Vec<String> = s.units.iter().flat_map(|u| u.files.clone()).collect();
    assert!(owned.iter().all(|f| !f.starts_with("ui/dist/")), "{owned:?}");
    assert_eq!(s.code_files, 3);
}

#[test]
fn manifests_give_a_name_and_a_description() {
    let cargo = read_manifest("crates/a/Cargo.toml", "[package]\nname = \"a\"\ndescription = 'Alpha'\n[dependencies]\nname = \"no\"\n").unwrap();
    assert_eq!((cargo.dir.as_str(), cargo.name.as_deref(), cargo.description.as_deref()), ("crates/a", Some("a"), Some("Alpha")));
    assert_eq!(read_manifest("Cargo.toml", "[workspace]\nmembers = []\n"), None, "a workspace alone is no package");
    let npm = read_manifest("web/package.json", r#"{"name": "@shop/web", "description": "The storefront"}"#).unwrap();
    assert_eq!(npm.name.as_deref(), Some("@shop/web"));
    let pom = read_manifest("pom.xml", "<project><parent><artifactId>boot</artifactId></parent><artifactId>shop</artifactId><description>Shop</description></project>").unwrap();
    assert_eq!((pom.name.as_deref(), pom.description.as_deref()), (Some("shop"), Some("Shop")));
    let go = read_manifest("go.mod", "module example.com/shop\n\ngo 1.22\n").unwrap();
    assert_eq!(go.name.as_deref(), Some("example.com/shop"));
}

#[test]
fn the_answer_schema_names_every_field_the_prompt_asks_for() {
    let schema = fill_schema();
    let required: Vec<&str> = schema["required"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    for field in &required {
        assert!(ANSWER.contains(&format!("\"{field}\"")), "{field}");
    }
    assert_eq!(required.len(), 10);
    assert_eq!(language_name("zh-CN"), "Simplified Chinese (简体中文)");
    assert!(language_ok("zh-CN") && !language_ok("zh;rm") && !language_ok("x"));
    assert!(language_rule(None).contains("English"));
}
