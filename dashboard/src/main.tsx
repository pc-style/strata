import { useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { parse, stringify, isInteger } from "lossless-json";
import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "./style.css";

type IndexInfo = {
  name: string;
  documents: number;
  deleted_documents: number;
  segments: number;
  generation: number;
  failed: boolean;
  mappings: { properties: Record<string, { type: string }> };
};
type Hit = { _id: string; _score: number; _source: Record<string, unknown> };
type Result = {
  hits: { total: { value: number }; hits: Hit[] };
  search_after: string | null;
  generation: number;
};
type Trace = {
  path: string;
  status: number;
  ms: number;
  request: unknown;
  response: unknown;
};
const pretty = (value: unknown) => stringify(value, null, 2);

async function api(path: string, body?: unknown) {
  const start = performance.now();
  const response = await fetch(
    path,
    body === undefined
      ? undefined
      : {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: stringify(body),
        },
  );
  const text = await response.text();
  let data: unknown;
  try {
    data = parse(text, null, {
      parseNumber: (value) =>
        isInteger(value) && !Number.isSafeInteger(Number(value))
          ? BigInt(value)
          : Number(value),
    });
  } catch {
    data = { error: text || response.statusText };
  }
  return {
    path,
    status: response.status,
    ms: performance.now() - start,
    request: body,
    response: data,
  };
}

function App() {
  const [indexes, setIndexes] = useState<IndexInfo[]>([]);
  const [index, setIndex] = useState("");
  const [field, setField] = useState("");
  const [query, setQuery] = useState("");
  const [dev, setDev] = useState(false);
  const [jsonMode, setJsonMode] = useState(false);
  const [json, setJson] = useState('{"query":{"match_all":{}},"size":10}');
  const [result, setResult] = useState<Result | null>(null);
  const [trace, setTrace] = useState<Trace | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [lastRequest, setLastRequest] = useState<Record<
    string,
    unknown
  > | null>(null);
  const sequence = useRef(0);
  const selected = indexes.find((item) => item.name === index);
  const fields = Object.entries(selected?.mappings.properties ?? {})
    .filter(([, value]) => value.type === "text")
    .map(([name]) => name);
  const reset = () => {
    sequence.current++;
    setResult(null);
    setTrace(null);
    setError("");
    setLastRequest(null);
  };

  async function load() {
    setBusy(true);
    setError("");
    try {
      const response = await api("/_indexes");
      if (response.status !== 200)
        throw new Error(`Cannot load indexes (HTTP ${response.status}).`);
      const items = (response.response as { indexes: IndexInfo[] }).indexes;
      setIndexes(items);
      setLoaded(true);
      if (!items.some((item) => item.name === index)) {
        const first = items[0];
        setIndex(first?.name ?? "");
        setField(
          Object.entries(first?.mappings.properties ?? {}).find(
            ([, value]) => value.type === "text",
          )?.[0] ?? "",
        );
      }
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  }
  useEffect(() => {
    void load();
  }, []);

  async function search(next = false) {
    const current = ++sequence.current;
    setBusy(true);
    setError("");
    setTrace(null);
    try {
      let body: Record<string, unknown>;
      if (next && lastRequest && result?.search_after)
        body = { ...lastRequest, search_after: result.search_after };
      else if (dev && jsonMode) {
        const parsed: unknown = parse(json);
        if (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
          throw new Error("Search body must be a JSON object.");
        body = parsed as Record<string, unknown>;
      } else {
        if (query.trim() && !field)
          throw new Error(
            "This index has no text fields. Use a JSON query in dev mode.",
          );
        body = {
          query: query.trim()
            ? { match: { [field]: query.trim() } }
            : { match_all: {} },
          size: 10,
        };
      }
      if (!next) setResult(null);
      const response = await api(`/${encodeURIComponent(index)}/_search`, body);
      if (current !== sequence.current) return;
      setTrace(response);
      if (response.status !== 200)
        throw new Error(
          (response.response as { error?: string }).error ??
            `HTTP ${response.status}`,
        );
      setResult(response.response as Result);
      setLastRequest(body);
    } catch (e) {
      if (current === sequence.current) {
        setResult(null);
        setError(e instanceof Error ? e.message : String(e));
      }
    } finally {
      if (current === sequence.current) setBusy(false);
    }
  }

  return (
    <main>
      <header className="flex items-center justify-between gap-4">
        <div className="flex items-baseline gap-3">
          <h1>Strata</h1>
          <span className="muted">Search</span>
        </div>
        <label className="toggle">
          <input
            type="checkbox"
            checked={dev}
            disabled={busy}
            onChange={(e) => {
              setDev(e.target.checked);
              if (jsonMode) {
                setJsonMode(false);
                reset();
              }
            }}
          />
          Dev mode
        </label>
      </header>
      <section aria-label="Search workspace">
        <div className="toolbar flex flex-wrap items-end gap-3">
          <label>
            Index
            <select
              value={index}
              disabled={busy || !indexes.length}
              onChange={(e) => {
                const name = e.target.value;
                setIndex(name);
                setField(
                  Object.entries(
                    indexes.find((i) => i.name === name)?.mappings.properties ??
                      {},
                  ).find(([, v]) => v.type === "text")?.[0] ?? "",
                );
                reset();
              }}
            >
              <option value="" disabled>
                Select an index
              </option>
              {indexes.map((item) => (
                <option key={item.name} value={item.name}>
                  {item.name}
                  {item.failed ? " (storage error)" : ""}
                </option>
              ))}
            </select>
          </label>
          <button
            disabled={busy}
            onClick={() => {
              reset();
              void load();
            }}
          >
            Reload indexes
          </button>
          {selected && (
            <span className="muted count">
              {selected.documents.toLocaleString()} searchable documents
            </span>
          )}
        </div>
        {loaded && !indexes.length ? (
          <div className="empty">
            <h2>No indexes yet</h2>
            <p>
              Create an index and add documents through the API, then reload
              this list.
            </p>
            <details>
              <summary>Show a minimal example</summary>
              <pre>{`curl -X PUT ${location.origin}/books -H 'Content-Type: application/json' -d '{"mappings":{"properties":{"title":{"type":"text"}}}}'
curl -X PUT '${location.origin}/books/_doc/1?refresh=true' -H 'Content-Type: application/json' -d '{"title":"Rust in Action"}'`}</pre>
            </details>
          </div>
        ) : (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void search();
            }}
          >
            <fieldset disabled={busy || !selected || selected.failed}>
              {dev && (
                <label className="toggle mode">
                  <input
                    type="checkbox"
                    checked={jsonMode}
                    onChange={(e) => {
                      setJsonMode(e.target.checked);
                      reset();
                    }}
                  />
                  JSON query
                </label>
              )}
              {dev && jsonMode ? (
                <label>
                  Search body
                  <textarea
                    rows={6}
                    spellCheck={false}
                    value={json}
                    onChange={(e) => {
                      setJson(e.target.value);
                      reset();
                    }}
                  />
                </label>
              ) : (
                <div className="searchline">
                  <label>
                    Text field
                    <select
                      value={field}
                      onChange={(e) => {
                        setField(e.target.value);
                        reset();
                      }}
                    >
                      <option value="">All documents only</option>
                      {fields.map((name) => (
                        <option key={name}>{name}</option>
                      ))}
                    </select>
                  </label>
                  <label className="grow">
                    Query
                    <input
                      type="search"
                      value={query}
                      placeholder="Leave empty to browse all documents"
                      onChange={(e) => {
                        setQuery(e.target.value);
                        reset();
                      }}
                    />
                  </label>
                </div>
              )}
              <div className="flex items-center gap-4 mt-4">
                <button className="primary" type="submit">
                  {busy ? "Searching…" : "Search"}
                </button>
                <span className="muted">
                  Committed writes appear after an API refresh.
                </span>
              </div>
            </fieldset>
          </form>
        )}
        {selected?.failed && (
          <p role="alert" className="error">
            This index stopped after a storage error. Check the server logs.
          </p>
        )}
        {error && (
          <p role="alert" className="error">
            {error}
          </p>
        )}
        <div role="status" aria-live="polite" className="result-status">
          {busy
            ? "Loading…"
            : result
              ? `${result.hits.total.value.toLocaleString()} matches · ${result.hits.hits.length} shown`
              : selected
                ? "Run a search to inspect your documents."
                : ""}
        </div>
        {result && (
          <section aria-label="Search results">
            {!result.hits.hits.length && (
              <p className="empty">
                No documents match. Try another query or check whether the index
                needs refreshing.
              </p>
            )}
            {result.hits.hits.map((hit) => (
              <article key={hit._id}>
                <div className="flex items-baseline justify-between gap-3">
                  <h2>
                    {typeof hit._source.title === "string"
                      ? hit._source.title
                      : hit._id}
                  </h2>
                  <span className="muted">{hit._id}</span>
                </div>
                <p className="preview">
                  {Object.entries(hit._source)
                    .filter(([key]) => key !== "title")
                    .map(
                      ([key, value]) =>
                        `${key}: ${typeof value === "string" ? value : stringify(value)}`,
                    )
                    .join(" · ")}
                </p>
                <details>
                  <summary>
                    Document{dev ? ` · score ${hit._score.toFixed(4)}` : ""}
                  </summary>
                  <pre>{pretty(hit._source)}</pre>
                </details>
              </article>
            ))}
            {result.search_after && (
              <button
                className="next"
                disabled={busy}
                onClick={() => void search(true)}
              >
                Next page
              </button>
            )}
          </section>
        )}
      </section>
      {dev && (
        <section className="inspector" aria-label="Developer inspection">
          <h2>Dev inspection</h2>
          <p className="muted">
            Index statistics reflect the loaded search snapshot. Reload to
            update.
          </p>
          {selected && (
            <dl className="stats">
              <div>
                <dt>Documents</dt>
                <dd>{selected.documents}</dd>
              </div>
              <div>
                <dt>Deleted slots</dt>
                <dd>{selected.deleted_documents}</dd>
              </div>
              <div>
                <dt>Segments</dt>
                <dd>{selected.segments}</dd>
              </div>
              <div>
                <dt>Generation</dt>
                <dd>{selected.generation}</dd>
              </div>
            </dl>
          )}
          {trace && (
            <p className="timing">
              HTTP {trace.status} · {trace.ms.toFixed(1)} ms browser round-trip
              · {trace.path}
              {result && ` · search generation ${result.generation}`}
            </p>
          )}
          <div className="json-grid">
            {selected && (
              <details open>
                <summary>Mappings</summary>
                <pre>{pretty(selected.mappings)}</pre>
              </details>
            )}
            {trace && (
              <>
                <details open>
                  <summary>Request</summary>
                  <pre>{pretty(trace.request)}</pre>
                </details>
                <details>
                  <summary>Response</summary>
                  <pre>{pretty(trace.response)}</pre>
                </details>
              </>
            )}
          </div>
        </section>
      )}
      <footer>Single node. Local access. No authentication.</footer>
    </main>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
