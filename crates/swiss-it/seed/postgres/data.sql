-- The PostgreSQL seed, data (SPEC §testing.it). Neutral names only - SPEC §release.hygiene applies
-- to seeds exactly as it applies to screenshots.

INSERT INTO users (id, name, email, status, balance, flags, avatar, big, born_at, is_active) VALUES
(1, '张三 🙂', 'one@example.test', 'active', 120.5000, '{"level": 3, "beta": true}', '\x89504e470d0a1a0a', 9223372036854775807, '1990-06-15 08:30:00.123456+00', TRUE),
(2, 'Ada Lovelace', NULL, 'idle', -0.0001, NULL, NULL, 0, '1970-01-01 00:00:00+00', FALSE),
(3, 'Zoë “quoted” Ñ', 'three@example.test', 'active', 99999999.9999, '[]', '\x00ff10', 9223372036854775806, '2024-02-29 23:59:59.999999+00', TRUE),
(4, '李四', 'four@example.test', 'banned', 0.0000, '{"m": null}', NULL, 1, '2000-01-01 00:00:00.000001+00', FALSE),
(5, 'Íñigo María', 'five@example.test', 'active', 42.4242, '{"k": [1, 2, 3]}', NULL, 2, '2010-07-04 12:00:00.500000+00', TRUE),
(6, '日本語のなまえ', NULL, 'idle', 7.0001, '{}', NULL, 4611686018427387904, '1999-12-31 23:59:59.999999+00', FALSE),
(7, 'naïve résumé', 'seven@example.test', 'active', 1000000.0001, '{"nested": {"deep": [true, false, null]}}', '\x0a', 9007199254740993, '2020-02-02 02:02:02.020202+00', TRUE),
(8, 'و عربي', 'eight@example.test', 'banned', -999.9999, NULL, NULL, 3, '1980-11-30 06:15:30.250000+00', FALSE);
SELECT setval('users_id_seq', 8, TRUE);

INSERT INTO orders (user_id, seq, note, amount, placed) VALUES
(1, 1, 'first', 10.00, '2026-01-02 09:00:00.000010+00'),
(1, 2, NULL, 20.50, '2026-01-03 10:30:00.123456+00'),
(1, 3, 'third note with, comma', 0.01, '2026-02-01 11:00:00.999999+00'),
(2, 1, NULL, -5.25, '2026-03-15 08:45:00+00'),
(3, 1, 'bulk', 1000000.00, '2026-04-01 00:00:00.000001+00'),
(4, 1, '返礼品', 88.88, '2026-05-05 05:05:05.555555+00');

-- Two byte-identical rows on purpose: page cursors and dedup have to earn their keep.
INSERT INTO events (at, kind, detail) VALUES
('2026-06-01 12:00:00+00', 'tick', '{"n": 1}'),
('2026-06-01 12:00:00+00', 'tick', '{"n": 1}'),
('2026-06-02 13:30:00.5+00', 'tock', NULL),
('2026-06-03 14:00:00.75+00', 'tock', '{}');

INSERT INTO wide VALUES
('w01', 6, NULL, NULL, NULL, 6.25, NULL, NULL, NULL, 'w10', NULL, 12.25, NULL, 42, NULL, NULL, NULL, 18.25, 'w19', NULL, NULL, NULL, NULL, 24.25, NULL, 78, NULL, 'w28', NULL, 30.25, NULL, NULL, NULL, NULL, NULL, 36.25, 'w37', 114, NULL, NULL, NULL, 42.25, NULL, NULL, NULL, 'w46', NULL, 48.25, NULL, 150, NULL, NULL, NULL, 54.25, 'w55', NULL, NULL, NULL, NULL, 60.25),
('w01', 6, NULL, NULL, NULL, 6.25, NULL, NULL, NULL, NULL, NULL, 12.25, NULL, 42, NULL, NULL, NULL, 18.25, 'w19', NULL, NULL, NULL, NULL, 24.25, NULL, 78, NULL, 'w28', NULL, NULL, NULL, NULL, NULL, NULL, NULL, 36.25, 'w37', 114, NULL, NULL, NULL, 42.25, NULL, NULL, NULL, 'w46', NULL, 48.25, NULL, NULL, NULL, NULL, NULL, 54.25, NULL, NULL, NULL, NULL, NULL, NULL),
('w01', 6, NULL, NULL, NULL, 6.25, NULL, NULL, NULL, NULL, NULL, 12.25, NULL, 42, NULL, NULL, NULL, 18.25, 'w19', NULL, NULL, NULL, NULL, NULL, NULL, 78, NULL, 'w28', NULL, 30.25, NULL, NULL, NULL, NULL, NULL, 36.25, 'w37', NULL, NULL, NULL, NULL, 42.25, NULL, NULL, NULL, 'w46', NULL, 48.25, NULL, 150, NULL, NULL, NULL, 54.25, 'w55', NULL, NULL, NULL, NULL, 60.25);

INSERT INTO big_rows (n, label, qty, price)
SELECT g, 'row-' || lpad(g::text, 4, '0'), (g * 7) % 13, (g % 500) * 1.25
FROM generate_series(1, 1000) AS g;

INSERT INTO app.documents (id, title, tags, created_by) VALUES
('00000000-0000-4000-8000-000000000001', '第一份文档', '{draft}', 1),
('00000000-0000-4000-8000-000000000002', 'invoice-42', '{final,accounting}', 3),
('00000000-0000-4000-8000-000000000003', 'notes', '{}', 1),
('00000000-0000-4000-8000-000000000004', '日本語タグ', '{tag-2,tag-1}', 7);

INSERT INTO audit.log_entries (doc, at, note) VALUES
('00000000-0000-4000-8000-000000000001', '2026-06-01 12:00:00+00', 'created'),
('00000000-0000-4000-8000-000000000002', '2026-06-01 13:00:00+00', NULL),
('00000000-0000-4000-8000-000000000002', '2026-06-02 14:30:00+00', 'finalised'),
('00000000-0000-4000-8000-000000000003', '2026-06-03 09:15:00+00', NULL),
('00000000-0000-4000-8000-000000000004', '2026-06-04 16:45:00+00', '日本語メモ');
