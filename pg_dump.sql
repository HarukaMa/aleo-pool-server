--
-- PostgreSQL database dump
--

-- Dumped from database version 15.1
-- Dumped by pg_dump version 15.0

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
-- Name: pay_solution(integer); Type: PROCEDURE; Schema: pool; Owner: -
--

CREATE PROCEDURE pool.pay_solution(IN solution_id_arg integer)
    LANGUAGE plpython3u
    AS $_$solution = plpy.execute(f"SELECT * FROM solution WHERE id = {solution_id_arg}", 1)
if solution.nrows() == 0:
  plpy.error("Solution id does not exist")
if solution[0]["paid"]:
  plpy.error("Solution already paid")
solution_id = solution[0]["id"]
shares = plpy.execute(f"SELECT * FROM share WHERE solution_id = {solution_id}")
if shares.nrows() == 0:
  plpy.fatal("No share data for solution")
data = {}
for share in shares:
  data[share["address"]] = share["share"]
raw_reward = solution[0]["reward"]
reward = int(raw_reward * 0.995)
total_shares = sum(data.values())
reward_per_share = reward // total_shares
def get_plan(name, stmt, types):
  if name in SD:
    return SD[name]
  plan = plpy.prepare(stmt, types)
  SD[name] = plan
  return plan
payout_plan = get_plan("payout_plan", "INSERT INTO payout (solution_id, address, amount) VALUES ($1, $2, $3)", ["integer", "text", "bigint"])
balance_plan = get_plan("balance_plan", "INSERT INTO balance (address, unpaid) VALUES ($1, $2) ON CONFLICT (address) DO UPDATE SET unpaid = balance.unpaid + $2", ["text", "bigint"])
stats_plan = get_plan("stats_plan", "INSERT INTO stats (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = stats.value + $2", ["text", "bigint"])
solution_plan = get_plan("block_plan", "UPDATE solution SET paid = true WHERE id = $1", ["integer"])
try:
  with plpy.subtransaction():
    paid = 0
    for miner, share in data.items():
      amount = reward_per_share * share
      payout_plan.execute([solution_id, miner, amount])
      balance_plan.execute([miner, amount])
      solution_plan.execute([solution_id])
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
-- Name: solution; Type: TABLE; Schema: pool; Owner: -
--

CREATE TABLE pool.solution (
    id integer NOT NULL,
    height bigint,
    reward bigint,
    "timestamp" bigint DEFAULT EXTRACT(epoch FROM now()) NOT NULL,
    paid boolean DEFAULT false NOT NULL,
    valid boolean DEFAULT false NOT NULL,
    commitment text NOT NULL,
    checked integer DEFAULT 0 NOT NULL
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

ALTER SEQUENCE pool.block_id_seq OWNED BY pool.solution.id;


--
-- Name: payout; Type: TABLE; Schema: pool; Owner: -
--

CREATE TABLE pool.payout (
    id integer NOT NULL,
    solution_id integer NOT NULL,
    address text NOT NULL,
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
    solution_id integer NOT NULL,
    address text NOT NULL,
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
-- Name: payout id; Type: DEFAULT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.payout ALTER COLUMN id SET DEFAULT nextval('pool.payout_id_seq'::regclass);


--
-- Name: share id; Type: DEFAULT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.share ALTER COLUMN id SET DEFAULT nextval('pool.share_id_seq'::regclass);


--
-- Name: solution id; Type: DEFAULT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.solution ALTER COLUMN id SET DEFAULT nextval('pool.block_id_seq'::regclass);


--
-- Name: balance balance_pk; Type: CONSTRAINT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.balance
    ADD CONSTRAINT balance_pk PRIMARY KEY (id);


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
-- Name: solution solution_pk; Type: CONSTRAINT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.solution
    ADD CONSTRAINT solution_pk PRIMARY KEY (id);


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
-- Name: payout_address_index; Type: INDEX; Schema: pool; Owner: -
--

CREATE INDEX payout_address_index ON pool.payout USING btree (address);


--
-- Name: solution_height_index; Type: INDEX; Schema: pool; Owner: -
--

CREATE INDEX solution_height_index ON pool.solution USING btree (height);


--
-- Name: solution_paid_index; Type: INDEX; Schema: pool; Owner: -
--

CREATE INDEX solution_paid_index ON pool.solution USING btree (paid);


--
-- Name: solution_valid_index; Type: INDEX; Schema: pool; Owner: -
--

CREATE INDEX solution_valid_index ON pool.solution USING btree (valid);


--
-- Name: payout payout_solution_id_fk; Type: FK CONSTRAINT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.payout
    ADD CONSTRAINT payout_solution_id_fk FOREIGN KEY (solution_id) REFERENCES pool.solution(id);


--
-- Name: share share_solution_id_fk; Type: FK CONSTRAINT; Schema: pool; Owner: -
--

ALTER TABLE ONLY pool.share
    ADD CONSTRAINT share_solution_id_fk FOREIGN KEY (solution_id) REFERENCES pool.solution(id);


--
-- PostgreSQL database dump complete
--

