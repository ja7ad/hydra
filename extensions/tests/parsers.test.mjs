// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//
// HLS/DASH manifest parsing, against the shapes real players emit.
// Run from the repository root:  node extensions/tests/parsers.test.mjs
import { readFileSync } from "node:fs";
const src = readFileSync("extensions/chrome/background.js", "utf8");

// Pull the pure parser functions out of the service worker source.
const names = ["drmName","absUrl","streamKey","streamVariantId","hlsAttrs","tagValue","parseHls","isoDuration","parseDash","classifyManifest"];
let code = "";
for (const n of names) {
  const i = src.indexOf(`function ${n}(`);
  if (i < 0) throw new Error("missing " + n);
  // brace-match to the closing brace at column 0
  const end = src.indexOf("\n}\n", i);
  code += src.slice(i, end + 3) + "\n";
}
const api = new Function(code + `return {${names.join(",")}};`)();

let fails = 0;
const eq = (label, got, want) => {
  const g = JSON.stringify(got), w = JSON.stringify(want);
  if (g !== w) { fails++; console.log(`FAIL ${label}\n  got  ${g}\n  want ${w}`); }
  else console.log(`ok   ${label} = ${g}`);
};

// ---- HLS master (Apple bipbop shape)
const master = `#EXTM3U
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud1",NAME="English",DEFAULT=YES,URI="a1/prog_index.m3u8"
#EXT-X-STREAM-INF:BANDWIDTH=6221600,AVERAGE-BANDWIDTH=5000000,RESOLUTION=1920x1080,FRAME-RATE=60.000,CODECS="avc1.640028,mp4a.40.2",AUDIO="aud1"
v8/prog_index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2312764,RESOLUTION=1280x720,CODECS="avc1.64001f,mp4a.40.2"
v5/prog_index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=902298,RESOLUTION=640x360,CODECS="avc1.64001e,mp4a.40.2"
v2/prog_index.m3u8
`;
const m = api.parseHls(master, "https://ex.com/hls/master.m3u8");
eq("master kind", m.kind, "master");
eq("master variants", m.variants.length, 3);
eq("master top res", [m.variants[0].width, m.variants[0].height], [1920,1080]);
eq("master top bw (AVERAGE wins)", m.variants[0].bandwidth, 5000000);
eq("master codecs kept whole", m.variants[0].codecs, "avc1.640028,mp4a.40.2");
eq("master frame rate", m.variants[0].frameRate, 60);
eq("variants sorted desc", m.variants.map(v=>v.bandwidth), [5000000,2312764,902298]);
eq("master audio renditions", m.audioTracks, 1);
eq("children resolved", m.children.includes("https://ex.com/hls/v8/prog_index.m3u8"), true);
eq("audio child too", m.children.includes("https://ex.com/hls/a1/prog_index.m3u8"), true);

// ---- HLS media playlist, VOD, AES-128
const media = `#EXTM3U
#EXT-X-TARGETDURATION:10
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-KEY:METHOD=AES-128,URI="https://ex.com/key",IV=0x0
#EXTINF:10.0,
seg1.ts
#EXTINF:9.5,
seg2.ts
#EXT-X-ENDLIST
`;
const md = api.parseHls(media, "https://ex.com/hls/v8/prog_index.m3u8");
eq("media kind", md.kind, "media");
eq("media duration", md.duration, 19.5);
eq("media segments", md.segments, 2);
eq("media not live", md.live, false);
eq("aes-128 is encryption not drm", [md.encryption, md.drm], ["AES-128", null]);

// ---- HLS live (no ENDLIST, no PLAYLIST-TYPE)
eq("live detected", api.parseHls(`#EXTM3U\n#EXTINF:6,\ns.ts\n`, "https://e/x.m3u8").live, true);

// ---- HLS FairPlay / Widevine
eq("fairplay drm", api.parseHls(`#EXTM3U\n#EXT-X-SESSION-KEY:METHOD=SAMPLE-AES,KEYFORMAT="com.apple.streamingkeydelivery",URI="skd://x"\n`, "https://e/x.m3u8").drm, "FairPlay");
eq("widevine drm", api.parseHls(`#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,KEYFORMAT="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed",URI="data:x"\n`, "https://e/x.m3u8").drm, "Widevine");
eq("METHOD=NONE is not drm", api.parseHls(`#EXTM3U\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n`, "https://e/x.m3u8").drm, null);

// ---- DASH
const mpd = `<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT1H2M3.5S">
 <Period>
  <AdaptationSet contentType="video" mimeType="video/mp4">
   <Representation id="v0" bandwidth="8000000" width="3840" height="2160" codecs="avc1.640033" frameRate="30"/>
   <Representation id="v1" bandwidth="3000000" width="1920" height="1080" codecs="avc1.64002a"/>
  </AdaptationSet>
  <AdaptationSet contentType="audio" mimeType="audio/mp4">
   <Representation id="a0" bandwidth="128000" codecs="mp4a.40.2"/>
  </AdaptationSet>
  <AdaptationSet contentType="text"><Representation id="t0" bandwidth="1000"/></AdaptationSet>
 </Period>
</MPD>`;
const d = api.parseDash(mpd, "https://ex.com/dash/m.mpd");
eq("dash duration PT1H2M3.5S", d.duration, 3723.5);
eq("dash video reps only", d.variants.map(v=>v.id), ["v0","v1"]);
eq("dash top res", [d.variants[0].width, d.variants[0].height], [3840,2160]);
eq("dash frameRate", d.variants[0].frameRate, 30);
eq("dash audio counted", d.audioTracks, 1);
eq("dash static not live", d.live, false);
eq("dash no drm", d.drm, null);

eq("dash dynamic is live", api.parseDash(`<MPD type="dynamic"></MPD>`, "u").live, true);
eq("dash widevine", api.parseDash(`<MPD><ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/></MPD>`,"u").drm, "Widevine");
eq("dash playready", api.parseDash(`<MPD><ContentProtection schemeIdUri="urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95"/></MPD>`,"u").drm, "PlayReady");
eq("dash cenc-only still blocked", api.parseDash(`<MPD><ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cenc"/></MPD>`,"u").drm, "CENC");

// ---- misc
eq("classify hls", api.classifyManifest("#EXTM3U\n"), "hls");
eq("classify dash", api.classifyManifest('<?xml version="1.0"?><MPD >'), "dash");
eq("classify neither", api.classifyManifest("<html>"), null);
eq("streamKey drops token", api.streamKey("https://e.com/a/m.m3u8?token=abc"), "https://e.com/a/m.m3u8");
eq("isoDuration PT30S", api.isoDuration("PT30S"), 30);
eq("isoDuration P1DT2H", api.isoDuration("P1DT2H"), 93600);
eq("variantId stable", api.streamVariantId({id:"v0",url:"u",bandwidth:5}), "v0|u|5");

console.log(fails ? `\n${fails} FAILED` : "\nall passed");
process.exit(fails ? 1 : 0);
