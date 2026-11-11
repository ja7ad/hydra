// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//
// The PAC evaluator the Chromium sandbox page runs, and the helpers a proxy
// switcher's generated script calls.
// Run from the repository root:  node extensions/tests/pac.test.mjs
import { readFileSync } from "node:fs";
import vm from "node:vm";

let fails = 0;
const check = (label, cond, extra = "") => {
  if (cond) console.log(`ok   ${label}`);
  else { fails++; console.log(`FAIL ${label} ${extra}`); }
};

const posted = [];
const listeners = [];
const ctx = vm.createContext({ addEventListener: (type, fn) => type === "message" && listeners.push(fn) });
vm.runInContext(readFileSync("extensions/chrome/pac.js", "utf8"), ctx, { filename: "pac.js" });
const find = (script, url) => ctx.pacFindProxy(script, url, new URL(url).hostname);

// SwitchyOmega / ZeroOmega's generated shape: a rule list that resolves
// profile names ("+name") until it reaches a PAC answer.
const SWITCHY = `
var FindProxyForURL = function(init, profiles) {
  return function(url, host) {
    "use strict";
    var result = init, scheme = url.substr(0, url.indexOf(":"));
    do {
      if (!profiles[result]) return result;
      result = profiles[result];
      if (typeof result === "function") result = result(url, host, scheme);
    } while (typeof result !== "string" || result.charCodeAt(0) === 43);
    return result;
  };
}("+auto switch", {
  "+auto switch": function(url, host, scheme) {
    "use strict";
    if (isPlainHostName(host) || shExpMatch(host, "*.local")) return "DIRECT";
    if (isInNet(host, "10.0.0.0", "255.0.0.0")) return "DIRECT";
    if (/(?:^|\\.)google\\.com$/.test(host)) return "+proxy";
    if (shExpMatch(url, "http://*.example.org/files/*")) return "+proxy";
    if (dnsDomainIs(host, ".socks.example")) return "+socks";
    return "DIRECT";
  },
  "+proxy": function() { return "PROXY 127.0.0.1:8080; DIRECT"; },
  "+socks": function() { return "SOCKS5 127.0.0.1:1080; SOCKS 127.0.0.1:1080"; }
});`;

check("switchy: a rule's host goes to its profile", find(SWITCHY, "https://www.google.com/") === "PROXY 127.0.0.1:8080; DIRECT");
check("switchy: a URL wildcard rule matches on the full URL", find(SWITCHY, "http://cdn.example.org/files/a.zip") === "PROXY 127.0.0.1:8080; DIRECT");
check("switchy: and not outside its path", find(SWITCHY, "http://cdn.example.org/other/a.zip") === "DIRECT");
check("switchy: a domain rule picks the SOCKS profile", find(SWITCHY, "https://a.socks.example/") === "SOCKS5 127.0.0.1:1080; SOCKS 127.0.0.1:1080");
check("switchy: plain host names stay direct", find(SWITCHY, "http://nas/") === "DIRECT");
check("switchy: an IP literal in a private net stays direct", find(SWITCHY, "http://10.1.2.3/") === "DIRECT");
check("switchy: anything else falls through to DIRECT", find(SWITCHY, "https://example.net/") === "DIRECT");

// The classic PAC file shape: a top-level function declaration.
const CLASSIC = `function FindProxyForURL(url, host) {
  if (localHostOrDomainIs(host, "www.corp.example")) return "DIRECT";
  if (dnsDomainLevels(host) > 2) return "HTTPS secure.example:443";
  return "PROXY proxy.example:3128";
}`;
check("classic: localHostOrDomainIs takes the bare name", find(CLASSIC, "http://www/") === "DIRECT");
check("classic: dnsDomainLevels counts the dots", find(CLASSIC, "https://a.b.c.example/") === "HTTPS secure.example:443");
check("classic: the fallthrough", find(CLASSIC, "https://b.example/") === "PROXY proxy.example:3128");

// No DNS: a name never resolves, so a rule keyed on its address does not fire.
check("dns: a name does not resolve", ctx.dnsResolve("example.com") === null && !ctx.isInNet("example.com", "0.0.0.0", "0.0.0.0"));
check("dns: an IP literal resolves to itself", ctx.dnsResolve("192.168.1.9") === "192.168.1.9");
check("dns: isInNet compares under the mask", ctx.isInNet("192.168.1.9", "192.168.0.0", "255.255.0.0") && !ctx.isInNet("192.169.1.9", "192.168.0.0", "255.255.0.0"));
check("dns: convert_addr is the unsigned address", ctx.convert_addr("255.255.255.255") === 4294967295);
check("shExpMatch: ? is one character", ctx.shExpMatch("a1c", "a?c") && !ctx.shExpMatch("a12c", "a?c"));
check("shExpMatch: dots are literal", !ctx.shExpMatch("axcom", "a.com"));

// Time helpers, against today's clock.
const now = new Date();
const DAYS = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
const MONTHS = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
const today = DAYS[now.getDay()], tomorrow = DAYS[(now.getDay() + 1) % 7], yesterday = DAYS[(now.getDay() + 6) % 7];
check("weekdayRange: today", ctx.weekdayRange(today));
check("weekdayRange: a range around today", ctx.weekdayRange(yesterday, tomorrow));
check("weekdayRange: a range that excludes today", !ctx.weekdayRange(tomorrow, tomorrow));
check("weekdayRange: a wrapping range from tomorrow to yesterday excludes today", !ctx.weekdayRange(tomorrow, yesterday));
check("dateRange: this month", ctx.dateRange(MONTHS[now.getMonth()]));
check("dateRange: this year as a range", ctx.dateRange(now.getFullYear() - 1, now.getFullYear() + 1));
check("dateRange: day and month bounds", ctx.dateRange(1, "JAN", 31, "DEC"));
check("dateRange: next year is not now", !ctx.dateRange(now.getFullYear() + 1));
check("dateRange: an odd argument count is not a range", !ctx.dateRange(1, "JAN", 31));
check("timeRange: this hour", ctx.timeRange(now.getHours()));
check("timeRange: the whole day", ctx.timeRange(0, 0, 0, 23, 59, 59));
check("timeRange: an hour range that ends before this hour", now.getHours() === 0 || !ctx.timeRange(0, now.getHours()));
check("timeRange: GMT reads UTC", ctx.timeRange(now.getUTCHours(), "GMT"));

// The sandbox's message protocol: an answer or an error, never a throw.
const ask = (data) => listeners.forEach((fn) => fn({ data, source: { postMessage: (m) => posted.push(m) } }));
ask({ id: 1, script: CLASSIC, url: "https://b.example/", host: "b.example" });
ask({ id: 2, script: "throw new Error('boom')", url: "https://b.example/", host: "b.example" });
ask({ id: 3, script: "var x = 1;", url: "https://b.example/", host: "b.example" });
ask({ hello: "not a PAC request" });
check("sandbox: an answer comes back under its id", posted[0]?.id === 1 && posted[0]?.result === "PROXY proxy.example:3128", JSON.stringify(posted[0]));
check("sandbox: a throwing script is an error, not a crash", posted[1]?.id === 2 && /boom/.test(posted[1]?.error), JSON.stringify(posted[1]));
check("sandbox: a script without FindProxyForURL is an error", posted[2]?.id === 3 && !!posted[2]?.error, JSON.stringify(posted[2]));
check("sandbox: anything else on the channel is ignored", posted.length === 3);

console.log(fails ? `\n${fails} FAILED` : "\nall passed");
process.exit(fails ? 1 : 0);
