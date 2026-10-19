const numericFields = ["statusCode", "elapsedMs", "count", "code"];
const booleanFields = ["isAdmin", "canControl", "pending"];
const operations = new Set(["login", "streams", "records", "cages", "create_order", "order", "play", "feed", "water", "door", "unknown"]);
const outcomes = new Set(["started", "success", "failed", "cancelled", "denied", "pending", "accepted", "opened", "active", "closed", "refunded", "unknown"]);

function write(level, event, details = {}) {
  const fields = {};
  for (const key of numericFields) {
    if (typeof details[key] === "number" && Number.isFinite(details[key])) fields[key] = details[key];
  }
  for (const key of booleanFields) {
    if (typeof details[key] === "boolean") fields[key] = details[key];
  }
  if (operations.has(details.operation)) fields.operation = details.operation;
  if (outcomes.has(details.outcome)) fields.outcome = details.outcome;
  if (typeof details.requestId === "string" && /^req-\d+-\d+$/.test(details.requestId)) fields.requestId = details.requestId;
  const safeEvent = typeof event === "string" && /^[a-z][a-z0-9_.]{0,63}$/.test(event) ? event : "invalid_event";
  console[level](`[miniprogram] ${safeEvent}`, { timestamp: new Date().toISOString(), ...fields });
}

module.exports = {
  info: (event, details) => write("info", event, details),
  warn: (event, details) => write("warn", event, details),
  error: (event, details) => write("error", event, details),
};