--
-- PostgreSQL database dump
--

-- Dumped from database version 14.4 (Debian 14.4-1.pgdg120+1)
-- Dumped by pg_dump version 14.4

SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

--
-- Name: pool; Type: SCHEMA; Schema: -; Owner: -
--

CREATE SCHEMA pool;


--
-- Name: pay_block(integer); Type: PROCEDURE; Schema: pool; Owner: -
--

CREATE PROCEDURE pool.pay_block(IN block_id_arg integer)
    LANGUAGE plpython3u
    AS $_$block = plpy.execute(f"SELECT * FROM block WHERE id = {block_id_arg}", 1)
if block.nrows() == 0:
  plpy.error("Block id does not exist")
if block[0]["paid"]:
  plpy.error("Block already paid")
if not block[0]["is_canonical"]:
  plpy.error("Block is not canonical")
block_id = block[0]["id"]
shares = plpy.execute(f"SELECT * FROM share WHERE block_id = {block_id}")
if shares.nrows() == 0:
  plpy.fatal("No share data for block")
data = {}
for share in shares:
  data[share["miner"]] = share["share"]
raw_reward = block[0]["reward"]
reward = int(raw_reward * 0.995)
total_shares = sum(data.values())
reward_per_share = reward // total_shares
def get_plan(name, stmt, types):
  if name in SD:
    return SD[name]
  plan = plpy.prepare(stmt, types)
  SD[name] = plan
  return plan
payout_plan = get_plan("payout_plan", "INSERT INTO payout (block_id, miner, amount) VALUES ($1, $2, $3)", ["integer", "text", "bigint"])
balance_plan = get_plan("balance_plan", "INSERT INTO balance (address, unpaid) VALUES ($1, $2) ON CONFLICT (address) DO UPDATE SET unpaid = balance.unpaid + $2", ["text", "bigint"])
stats_plan = get_plan("stats_plan", "INSERT INTO stats (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = stats.value + $2", ["text", "bigint"])
block_plan = get_plan("block_plan", "UPDATE block SET paid = true WHERE id = $1", ["integer"])
try:
  with plpy.subtransaction():
    paid = 0
    for miner, share in data.items():
      amount = reward_per_share * share
      payout_plan.execute([block_id, miner, amount])
      balance_plan.execute([miner, amount])
      block_plan.execute([block_id])
      paid += amount
    stats_plan.execute(["total_paid", paid])
    stats_plan.execute(["total_fee", raw_reward - reward])
    stats_plan.execute(["total_rounding", reward - paid])
	
except plpy.SPIError as e:
  plpy.fatal(f"Error while updating database: {e.args}")
$_$;


SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: balance; Type: TABLE; Schema: pool; Owner: -
--

CREATE TABLE pool.balance (
    id integer NOT NULL,
    address text NOT NULL,
    unpaid bigint DEFAULT 0 NOT NULL,
    paid bigint DEFAULT 0 NOT NULL,
    pending bigint DEFAULT 0 NOT NULL
);


--
-- Name: balance_id_seq; Type: SEQUENCE; Schema: pool; Owner: -
--

CREATE SEQUENCE pool.balance_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: balance_id_seq; Type: SEQUENCE OWNED BY; Schema: pool; Owner: -
--

ALTER SEQUENCE pool.balance_id_seq OWNED BY pool.balance.id;


--
-- Name: block; Type: TABLE; Schema: pool; Owner: -
--

CREATE TABLE pool.block (
    id integer NOT NULL,
    height bigint NOT NULL,
    block_hash text NOT NULL,
    is_canonical boolean DEFAULT true,
    reward bigint NOT NULL,
    "timestamp" bigint DEFAULT EXTRACT(epoch FROM now()),
    paid boolean DEFAULT false NOT NULL,
    checked boolean DEFAULT false NOT NULL
);


--
-- Name: block_id_seq; Type: SEQUENCE; Schema: pool; Owner: -
--

CREATE SEQUENCE pool.block_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: block_id_seq; Type: SEQUENCE OWNED BY; Schema: pool; Owner: -
--

ALTER SEQUENCE pool.block_id_seq OWNED BY pool.block.id;


--
-- Name: payout; Type: TABLE; Schema: pool; Owner: -
--

CREATE TABLE pool.payout (
    id integer NOT NULL,
    block_id integer NOT NULL,
    miner text NOT NULL,
    amount bigint NOT NULL,
    "timestamp" integer DEFAULT EXTRACT(epoch FROM now())
);


--
-- Name: payout_id_seq; Type: SEQUENCE; Schema: pool; Owner: -
--

CREATE SEQUENCE pool.payout_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: payout_id_seq; Type: SEQUENCE OWNED BY; Schema: pool; Owner: -
--

ALTER SEQUENCE pool.payout_id_seq OWNED BY pool.payout.id;


--
-- Name: share; Type: TABLE; Schema: pool; Owner: -
--

CREATE TABLE pool.share (
    id integer NOT NULL,
    block_id integer NOT NULL,
    miner text NOT NULL,
    share bigint NOT NULL
);


--
-- Name: share_id_seq; Type: SEQUENCE; Schema: pool; Owner: -
--

CREATE SEQUENCE pool.share_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: share_id_seq; Type: SEQUENCE OWNED BY; Schema: pool; Owner: -
--

ALTER SEQUENCE pool.share_id_seq OWNED BY pool.share.id;


--
-- Name: stats; Type: TABLE; Schema: pool; Owner: -
--

CREATE TABLE pool.stats (
    key text NOT NULL,
    value bigint
);


--
-- Name: balance id; Type: DEFAULT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.balance ALTER COLUMN id SET DEFAULT nextval('pool.balance_id_seq'::regclass);


--
-- Name: block id; Type: DEFAULT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.block ALTER COLUMN id SET DEFAULT nextval('pool.block_id_seq'::regclass);


--
-- Name: payout id; Type: DEFAULT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.payout ALTER COLUMN id SET DEFAULT nextval('pool.payout_id_seq'::regclass);


--
-- Name: share id; Type: DEFAULT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.share ALTER COLUMN id SET DEFAULT nextval('pool.share_id_seq'::regclass);


--
-- Name: balance balance_pk; Type: CONSTRAINT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.balance
    ADD CONSTRAINT balance_pk PRIMARY KEY (id);


--
-- Name: block block_pk; Type: CONSTRAINT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.block
    ADD CONSTRAINT block_pk PRIMARY KEY (id);


--
-- Name: payout payout_pk; Type: CONSTRAINT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.payout
    ADD CONSTRAINT payout_pk PRIMARY KEY (id);


--
-- Name: share share_pk; Type: CONSTRAINT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.share
    ADD CONSTRAINT share_pk PRIMARY KEY (id);


--
-- Name: stats stats_pk; Type: CONSTRAINT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.stats
    ADD CONSTRAINT stats_pk PRIMARY KEY (key);


--
-- Name: balance_address_uindex; Type: INDEX; Schema: pool; Owner: -
--

CREATE UNIQUE INDEX balance_address_uindex ON pool.balance USING btree (address);


--
-- Name: block_block_hash_uindex; Type: INDEX; Schema: pool; Owner: -
--

CREATE UNIQUE INDEX block_block_hash_uindex ON pool.block USING btree (block_hash);


--
-- Name: block_checked_index; Type: INDEX; Schema: pool; Owner: -
--

CREATE INDEX block_checked_index ON pool.block USING btree (checked);


--
-- Name: block_height_index; Type: INDEX; Schema: pool; Owner: -
--

CREATE INDEX block_height_index ON pool.block USING btree (height);


--
-- Name: block_paid_index; Type: INDEX; Schema: pool; Owner: -
--

CREATE INDEX block_paid_index ON pool.block USING btree (paid);


--
-- Name: payout_miner_index; Type: INDEX; Schema: pool; Owner: -
--

CREATE INDEX payout_miner_index ON pool.payout USING btree (miner);


--
-- Name: payout payout_block_id_fk; Type: FK CONSTRAINT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.payout
    ADD CONSTRAINT payout_block_id_fk FOREIGN KEY (block_id) REFERENCES pool.block(id);


--
-- Name: share share_block_id_fk; Type: FK CONSTRAINT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.share
    ADD CONSTRAINT share_block_id_fk FOREIGN KEY (block_id) REFERENCES pool.block(id);


--
-- PostgreSQL database dump complete
--

