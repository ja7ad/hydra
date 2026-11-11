// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//
// A PAC script evaluator, run inside the manifest's sandbox page: the one
// place an MV3 extension may compile a string into code. It answers the
// question Chromium asks a proxy switcher's script (ZeroOmega, SwitchyOmega,
// a corporate wpad.dat) for one URL, and nothing else.
//
// There is no DNS here. A PAC helper that would resolve a name answers as if
// the name did not resolve, which is also what the browser does offline; an
// IP literal still resolves to itself, so `isInNet` keeps working for those.

const PAC_DAYS = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
const PAC_MONTHS = [
  "JAN",
  "FEB",
  "MAR",
  "APR",
  "MAY",
  "JUN",
  "JUL",
  "AUG",
  "SEP",
  "OCT",
  "NOV",
  "DEC",
];
const IPV4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/;

function convert_addr(ip) {
  const m = IPV4.exec(String(ip));
  return m ? ((m[1] << 24) | (m[2] << 16) | (m[3] << 8) | m[4]) >>> 0 : 0;
}

function dnsResolve(host) {
  const h = String(host);
  return IPV4.test(h) || h.includes(":") ? h : null;
}

function isInNet(host, pattern, mask) {
  const ip = dnsResolve(host);
  if (!ip || !IPV4.test(ip)) return false;
  const m = convert_addr(mask);
  return (convert_addr(ip) & m) >>> 0 === (convert_addr(pattern) & m) >>> 0;
}

function shExpMatch(str, pattern) {
  const rx = String(pattern)
    .replace(/[.+^${}()|[\]\\]/g, "\\$&")
    .replace(/\*/g, ".*")
    .replace(/\?/g, ".");
  return new RegExp(`^${rx}$`).test(String(str));
}

/// The clock a time helper reads, and its arguments without the trailing
/// "GMT" that switches it to UTC.
function pacClock(args) {
  const gmt = args.length > 0 && String(args.at(-1)).toUpperCase() === "GMT";
  const now = new Date();
  const get = (local, utc) => (gmt ? now[utc]() : now[local]());
  return {
    args: gmt ? args.slice(0, -1) : args,
    day: get("getDate", "getUTCDate"),
    month: get("getMonth", "getUTCMonth"),
    year: get("getFullYear", "getUTCFullYear"),
    weekday: get("getDay", "getUTCDay"),
    seconds:
      get("getHours", "getUTCHours") * 3600 +
      get("getMinutes", "getUTCMinutes") * 60 +
      get("getSeconds", "getUTCSeconds"),
  };
}

/// `lo <= v <= hi`, where a range whose end comes before its start wraps
/// around (FRI..MON, NOV..FEB, 22h..6h).
const inWrappedRange = (v, lo, hi) => (lo <= hi ? lo <= v && v <= hi : v >= lo || v <= hi);

function weekdayRange(...raw) {
  const { args, weekday } = pacClock(raw);
  const lo = PAC_DAYS.indexOf(String(args[0]).toUpperCase());
  const hi = args.length > 1 ? PAC_DAYS.indexOf(String(args[1]).toUpperCase()) : lo;
  return lo >= 0 && hi >= 0 && inWrappedRange(weekday, lo, hi);
}

// Each argument is a day (1-31), a month name or a four-digit year; the
// bounds compare as year-month-day numbers built from the fields given.
function dateRange(...raw) {
  const clock = pacClock(raw);
  const kinds = clock.args.map((a) => {
    const month = PAC_MONTHS.indexOf(String(a).toUpperCase());
    if (month >= 0) return ["month", month];
    const n = Number(a);
    return n > 31 ? ["year", n] : ["day", n];
  });
  if (kinds.length !== 1 && kinds.length % 2) return false;
  const half = kinds.length === 1 ? kinds : kinds.slice(0, kinds.length / 2);
  const end = kinds.length === 1 ? kinds : kinds.slice(kinds.length / 2);
  const weight = { year: 10000, month: 100, day: 1 };
  const value = (fields) => fields.reduce((sum, [k, v]) => sum + v * weight[k], 0);
  const now = value(half.map(([k]) => [k, clock[k]]));
  if (half.some(([k]) => k === "year")) return value(half) <= now && now <= value(end);
  return inWrappedRange(now, value(half), value(end));
}

// timeRange(h), timeRange(h1, h2), (h1, m1, h2, m2) or (h1, m1, s1, h2, m2, s2).
function timeRange(...raw) {
  const { args, seconds } = pacClock(raw);
  const n = args.map(Number);
  if (n.length === 1) return Math.floor(seconds / 3600) === n[0];
  const at = (h, m = 0, s = 0) => h * 3600 + m * 60 + s;
  if (n.length === 2) return inWrappedRange(seconds, at(n[0]), at(n[1]) - 1);
  if (n.length === 4) return inWrappedRange(seconds, at(n[0], n[1]), at(n[2], n[3]));
  if (n.length === 6) return inWrappedRange(seconds, at(n[0], n[1], n[2]), at(n[3], n[4], n[5]));
  return false;
}

const PAC_HELPERS = {
  isPlainHostName: (host) => !String(host).includes("."),
  dnsDomainIs: (host, domain) => String(host).toLowerCase().endsWith(String(domain).toLowerCase()),
  localHostOrDomainIs: (host, hostdom) => {
    const h = String(host).toLowerCase();
    const full = String(hostdom).toLowerCase();
    return h === full || (!h.includes(".") && full.startsWith(`${h}.`));
  },
  isResolvable: (host) => dnsResolve(host) !== null,
  isInNet,
  dnsResolve,
  myIpAddress: () => "127.0.0.1",
  dnsDomainLevels: (host) => String(host).split(".").length - 1,
  shExpMatch,
  weekdayRange,
  dateRange,
  timeRange,
  convert_addr,
  alert: () => {},
};

// A capture asks once per download and the popup once per open, nearly always
// with the same script: compiling a rule list of thousands of entries again
// each time is the whole cost of answering.
let compiled = { script: null, find: null };

/// What the script's `FindProxyForURL` returns for this URL, as a string.
function pacFindProxy(script, url, host) {
  if (compiled.script !== script) {
    const names = Object.keys(PAC_HELPERS);
    const find = new Function(
      ...names,
      `${script}\n;return typeof FindProxyForURL === "function" ? FindProxyForURL : null;`,
    )(...Object.values(PAC_HELPERS));
    if (typeof find !== "function") throw new Error("the script defines no FindProxyForURL");
    compiled = { script, find };
  }
  return String(compiled.find(url, host) ?? "");
}

globalThis.addEventListener?.("message", (event) => {
  const { id, script, url, host } = event.data || {};
  if (typeof id !== "number" || typeof script !== "string") return;
  let reply;
  try {
    reply = { id, result: pacFindProxy(script, url, host) };
  } catch (e) {
    reply = { id, error: String(e?.message || e) };
  }
  event.source?.postMessage(reply, "*");
});
