import "./styles.css";
import { NativeAsyncDuckDB } from "./native-duckdb.js";

const sql = document.querySelector("#sql");
const status = document.querySelector("#status");
const runButton = document.querySelector("#run-query");
const sampleButton = document.querySelector("#sample-query");
const rowCount = document.querySelector("#row-count");
const schema = document.querySelector("#schema");
const head = document.querySelector("#result-head");
const body = document.querySelector("#result-body");
const error = document.querySelector("#error");

const sampleSql = `SELECT
  customer_region,
  order_status,
  count(*) AS orders,
  round(avg(total_amount), 2) AS avg_order,
  round(sum(total_amount), 2) AS revenue
FROM orders
GROUP BY customer_region, order_status
ORDER BY customer_region, revenue DESC;`;

let conn;

runButton.addEventListener("click", () => runQuery());
sampleButton.addEventListener("click", () => {
  sql.value = sampleSql;
  runQuery();
});

boot();

async function boot() {
  setStatus("connecting");
  try {
    const db = await new NativeAsyncDuckDB().instantiate();
    conn = await db.connect();
    setStatus("ready");
    await runQuery();
  } catch (err) {
    setStatus("unavailable");
    showError(err);
  }
}

async function runQuery() {
  if (!conn) {
    return;
  }

  setBusy(true);
  clearError();

  try {
    const table = await conn.query(sql.value);
    renderTable(table);
    setStatus("ready");
  } catch (err) {
    showError(err);
    setStatus("error");
  } finally {
    setBusy(false);
  }
}

function renderTable(table) {
  const fields = table.schema.fields;
  const names = fields.map((field) => field.name);

  rowCount.textContent = `${table.numRows} ${table.numRows === 1 ? "row" : "rows"}`;
  schema.textContent = fields
    .map((field) => `${field.name}: ${String(field.type)}`)
    .join("  ");

  head.replaceChildren(row(
    "tr",
    names.map((name) => cell("th", name))
  ));

  const columns = names.map((name) => table.getChild(name));
  const rows = [];
  const limit = Math.min(table.numRows, 500);
  for (let index = 0; index < limit; index += 1) {
    rows.push(row(
      "tr",
      columns.map((column) => cell("td", formatValue(column?.get(index))))
    ));
  }
  body.replaceChildren(...rows);
}

function row(tag, children) {
  const element = document.createElement(tag);
  element.append(...children);
  return element;
}

function cell(tag, text) {
  const element = document.createElement(tag);
  element.textContent = text;
  return element;
}

function formatValue(value) {
  if (value == null) {
    return "";
  }
  if (typeof value === "bigint") {
    return value.toString();
  }
  if (value instanceof Uint8Array) {
    return `0x${Array.from(value.slice(0, 16), (byte) => byte.toString(16).padStart(2, "0")).join("")}`;
  }
  return String(value);
}

function setBusy(busy) {
  runButton.disabled = busy;
  sampleButton.disabled = busy;
  if (busy) {
    setStatus("running");
  }
}

function setStatus(value) {
  status.textContent = value;
}

function showError(err) {
  error.hidden = false;
  error.textContent = err instanceof Error ? err.message : String(err);
}

function clearError() {
  error.hidden = true;
  error.textContent = "";
}

