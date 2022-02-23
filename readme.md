# Aleo Mining Pool Server

## Introduction

A mining pool server for the Aleo network.

## Why a Standalone Server?

1. I wanted to separate the mining pool part from the network node as ledger syncing sometimes interferes with the mining pool operations.
2. I wanted to use a more efficient network protocol for pool - miner communication. 
3. Making too many changes to the snarkOS code could be a bad idea as I still need to sync with upstream code.
4. It's easier to test the mining pool with a standalone server.
5. It's also easier to add more features to a smaller codebase.

## Features

1. A stratum-like protocol for pool - miner communication.
2. A good enough automatic difficulty targeting system. (Needs more test under high load)
3. Stats for pool and provers.

## State

Still need to implement some more API endpoints and test if it could correctly handle found blocks.

The `0.3` branch of my prover is using the new protocol.

### Things to test

* ~~If new blocks can be properly generated~~
* ~~If the payout system works~~
* If the difficulty retargeting system works under high load - Works under light load
* If there are no deadlock under high load - Works under light load

## Usage

Don't use unless you know what you're doing for now.

## License

AGPL-3.0-or-later
