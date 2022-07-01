--
-- PostgreSQL database dump
--

-- Dumped from database version 14.4 (Debian 14.4-1.pgdg120+1)
-- Dumped by pg_dump version 14.2

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


SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: block; Type: TABLE; Schema: pool; Owner: -
--

CREATE TABLE pool.block (
    id integer NOT NULL,
    height bigint NOT NULL,
    block_hash text NOT NULL,
    is_canonical boolean DEFAULT true,
    reward bigint NOT NULL,
    "timestamp" bigint DEFAULT EXTRACT(epoch FROM now())
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

