-- The MySQL seed, schema (SPEC §testing.it). Every shape below is a trap the panel
-- or an adapter has already fallen into once; the seed guard test pins each one:
-- utf8mb4 text, an unsigned BIGINT past 2^53, DECIMAL precision, JSON, ENUM, BLOB,
-- microsecond DATETIME, a composite primary key with a foreign key, a PK-less table
-- with two byte-identical rows, a 60-column wide table, 1,000 bulk rows and a view.

CREATE TABLE users (
    id        BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    name      VARCHAR(120)    NOT NULL,
    email     VARCHAR(160)    NULL,
    status    ENUM('active','idle','banned') NOT NULL DEFAULT 'active',
    balance   DECIMAL(20,4)   NOT NULL,
    flags     JSON            NULL,
    avatar    BLOB            NULL,
    big       BIGINT UNSIGNED NOT NULL,
    born_at   DATETIME(6)     NOT NULL,
    is_active BOOLEAN         NOT NULL DEFAULT TRUE,
    PRIMARY KEY (id)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_0900_ai_ci;

CREATE TABLE orders (
    user_id BIGINT UNSIGNED NOT NULL,
    seq     INT             NOT NULL,
    note    TEXT            NULL,
    amount  DECIMAL(12,2)   NOT NULL,
    placed  DATETIME(6)     NOT NULL,
    PRIMARY KEY (user_id, seq),
    CONSTRAINT fk_orders_user FOREIGN KEY (user_id) REFERENCES users (id)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_0900_ai_ci;

-- No primary key on purpose: paging must survive byte-identical duplicate rows.
CREATE TABLE events (
    at     DATETIME(6) NOT NULL,
    kind   VARCHAR(40) NOT NULL,
    detail JSON        NULL
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_0900_ai_ci;

CREATE TABLE wide (
    c01 VARCHAR(80) NULL,
    c02 INT NULL,
    c03 DECIMAL(10,2) NULL,
    c04 VARCHAR(80) NULL,
    c05 INT NULL,
    c06 DECIMAL(10,2) NULL,
    c07 VARCHAR(80) NULL,
    c08 INT NULL,
    c09 DECIMAL(10,2) NULL,
    c10 VARCHAR(80) NULL,
    c11 INT NULL,
    c12 DECIMAL(10,2) NULL,
    c13 VARCHAR(80) NULL,
    c14 INT NULL,
    c15 DECIMAL(10,2) NULL,
    c16 VARCHAR(80) NULL,
    c17 INT NULL,
    c18 DECIMAL(10,2) NULL,
    c19 VARCHAR(80) NULL,
    c20 INT NULL,
    c21 DECIMAL(10,2) NULL,
    c22 VARCHAR(80) NULL,
    c23 INT NULL,
    c24 DECIMAL(10,2) NULL,
    c25 VARCHAR(80) NULL,
    c26 INT NULL,
    c27 DECIMAL(10,2) NULL,
    c28 VARCHAR(80) NULL,
    c29 INT NULL,
    c30 DECIMAL(10,2) NULL,
    c31 VARCHAR(80) NULL,
    c32 INT NULL,
    c33 DECIMAL(10,2) NULL,
    c34 VARCHAR(80) NULL,
    c35 INT NULL,
    c36 DECIMAL(10,2) NULL,
    c37 VARCHAR(80) NULL,
    c38 INT NULL,
    c39 DECIMAL(10,2) NULL,
    c40 VARCHAR(80) NULL,
    c41 INT NULL,
    c42 DECIMAL(10,2) NULL,
    c43 VARCHAR(80) NULL,
    c44 INT NULL,
    c45 DECIMAL(10,2) NULL,
    c46 VARCHAR(80) NULL,
    c47 INT NULL,
    c48 DECIMAL(10,2) NULL,
    c49 VARCHAR(80) NULL,
    c50 INT NULL,
    c51 DECIMAL(10,2) NULL,
    c52 VARCHAR(80) NULL,
    c53 INT NULL,
    c54 DECIMAL(10,2) NULL,
    c55 VARCHAR(80) NULL,
    c56 INT NULL,
    c57 DECIMAL(10,2) NULL,
    c58 VARCHAR(80) NULL,
    c59 INT NULL,
    c60 DECIMAL(10,2) NULL
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_0900_ai_ci;

CREATE TABLE big_rows (
    n     INT           NOT NULL,
    label VARCHAR(64)   NOT NULL,
    qty   INT           NOT NULL,
    price DECIMAL(12,2) NOT NULL,
    PRIMARY KEY (n)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_0900_ai_ci;

CREATE VIEW active_users AS
SELECT id, name, email, balance FROM users WHERE status = 'active';
