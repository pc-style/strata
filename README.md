# Strata

Single-node JSON search powered by the published [`lucene-rs`](https://crates.io/crates/lucene-rs) package.
**Strata 0.1.0 is unreleased.**
This is Elasticsearch-inspired, not Elasticsearch API or client compatible.
Strata owns the HTTP API, mappings and service lifecycle. The separate lucene-rs project
owns indexing, scoring, numeric ranges, doc values and sorting.

```toml
[dependencies]
lucene-rs = "0.2"
```

There is no copied engine, Git dependency, path dependency or registry override.
The committed lockfile resolves lucene-rs 0.2.0 from crates.io. Strata's versions are
independent of the engine's. This is an early single-node implementation, not a
production-ready Elasticsearch replacement; see the operational limits below.

## Run from source

```sh
cargo run --release --locked --bin strata -- --data ./strata-data --listen 127.0.0.1:9200
```

The binary accepts loopback addresses only. There is no authentication or TLS;
do not expose it directly to a network. One process owns each data directory.
Build Strata from source. Its manual-only release workflow creates a GitHub source release
after CI passes; it does not publish a crate or deploy a running service.

## Index, write, search

```sh
curl -X PUT localhost:9200/books -H 'Content-Type: application/json' -d '
{"mappings":{"properties":{"title":{"type":"text"},"price":{"type":"double"},"tag":{"type":"keyword"},"published":{"type":"date"}}}}'

curl -X PUT 'localhost:9200/books/_doc/one?refresh=true' -H 'Content-Type: application/json' -d '
{"title":"Rust in Action","price":29.5,"tag":"programming","published":1727740800000}'

curl -X POST localhost:9200/books/_search -H 'Content-Type: application/json' -d '
{"query":{"bool":{"must":[{"match":{"title":"rust"}}],"filter":[{"range":{"price":{"lt":40}}}]}},"sort":[{"field":"price","order":"asc","missing":"last"}],"size":10}'
```

Mappings are explicit and immutable: `text`, `keyword`, `long`, `double`, `date`.
Dates are signed 64-bit **epoch milliseconds**, not ISO strings. Values are scalar;
null is missing, arrays/objects and unmapped fields are rejected. Doubles must be finite.
Keywords compare by UTF-8 bytes; numbers retain their types (longs are not converted to doubles).

| Endpoint | Behavior |
|---|---|
| `GET /`, `GET /_health` | Version and process health (not a storage integrity check) |
| `PUT /{index}`, `GET /{index}` | Create an explicitly mapped index; read its mappings |
| `PUT /{index}/_doc/{id}` | Insert or completely replace a document |
| `GET /{index}/_doc/{id}` | Read the latest committed source |
| `DELETE /{index}/_doc/{id}` | Delete a document; absent IDs return 404 |
| `POST /{index}/_refresh` | Make committed writes visible to search |
| `POST /{index}/_search` | Query, sort, exact count and cursor pagination |
| `POST /{index}/_bulk` | NDJSON `index` (followed by source) and `delete` actions |

Successful writes commit synchronously. Search uses a stable reader until refresh;
GET uses the latest commit. Append `?refresh=true` to a write or bulk request to refresh
immediately. Restart opens the latest commit. There is no automatic refresh timer.

Queries: `match_all`, `term`, `match`, `match_phrase`, numeric/date `range` with
`gt/gte/lt/lte`, and `bool` with arrays of `must/should/filter/must_not` and integer
`minimum_should_match`. `match` analyzes text and ORs the resulting terms; `term` is
exact and accepts text/keyword fields only. Empty and pure-negative bool queries match
nothing, following Lucene. Omitted or null range bounds are unbounded.

Default order is score descending; explicit field sorting supports keyword/numeric/date
fields and up to eight sort keys. Missing placement is independent of sort direction;
ties use internal document ID. Pass the returned opaque `search_after` string with
the **same query and sort** to get another page. This is not Elasticsearch's sort-value
array protocol. Tokens expire after five minutes, may be evicted after 256 active tokens
per index, and are invalidated by a changed search snapshot or restart.
Strata owns this session policy and the 10,000-hit page limit. The engine only accepts
a stateless score/sort-value boundary. Strata counts matches separately for the first
page and retains that exact total with the cursor for subsequent pages of the same snapshot.

Bulk example (`Content-Type: application/x-ndjson`):

```json
{"index":{"_id":"two"}}
{"title":"Search engines","price":19.0}
{"delete":{"_id":"one"}}
```

Malformed NDJSON is rejected before writes. Mapping failures are per-item errors;
valid actions execute in order and commit together. Deleting an absent ID in bulk is
an idempotent success. A storage failure aborts the response and stops access to that
index until restart; this is not a general transaction or retry-deduplication API.

## Limits and implementation

- 128 indexes, 256 fields per mapping; index/field names are 1–64 lowercase ASCII
  letters, digits, `_` or `-`, not starting with `_`. Document IDs are 1–512 bytes.
- 2 MiB request bodies, 1,000 bulk operations, 16 KiB query JSON, query depth 24,
  256 clauses per bool, pages of 1–10,000 hits, 16 MiB combined returned source text.
- Requests run on blocking workers but serialize through one catalog mutex. Admission
  is bounded to 32 workers; excess work receives 429. This is not a throughput claim.
- Numeric columns and keyword doc values are single-valued, checksummed and loaded
  into RAM. Numeric range lookup uses a sorted in-memory index, **not Lucene BKD**.
- Sorted collection still scans all matches; the heap is page-sized, but range matching
  materializes document IDs. Initial pages also perform a separate exact-count operation.
  The existing engine `search` retains block pruning.
- No shards, replicas, distributed coordination, aggregations, vectors, index deletion,
  partial document updates, dynamic mappings, compressed doc values or multi-value sorting.

Engine 0.2 reads 0.1 indexes, but 0.1 cannot read newly written 0.2 segments. Back up
before upgrading; downgrade requires restoring that backup. Server data directories
also require mappings and internal source/ID fields: an arbitrary engine index is not
automatically a server index.

## Check

```sh
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```
