// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Safari native-messaging handler for Hydra.
//
// Safari routes `browser.runtime.sendNativeMessage(...)` from the web
// extension to this handler inside the wrapper app — there is no manifest
// registry like Chrome's. The job is identical to crates/hydra-host: add
// the token from <~/.config/hydra>/ipc.json, forward one JSON line over the
// loopback socket hydra-gui listens on, hand the reply line back.
//
// NOTE: scripts/build-safari-extension.sh disables the app sandbox for the
// extension target — reading ~/.config/hydra/ipc.json and launching the
// app are impossible from a sandboxed extension. Local use only; an App
// Store build would need an app group + XPC design instead.

import Foundation
import SafariServices

let extensionMessageKey = "message"

@available(macOS 11.0, *)
class SafariWebExtensionHandler: NSObject, NSExtensionRequestHandling {

    func beginRequest(with context: NSExtensionContext) {
        let item = context.inputItems.first as? NSExtensionItem
        let message: Any?
        if #available(macOS 11.0, *) {
            message =
                item?.userInfo?[SFExtensionMessageKey]
                ?? item?.userInfo?[extensionMessageKey]
        } else {
            message = item?.userInfo?[extensionMessageKey]
        }

        var request = (message as? [String: Any]) ?? [:]
        let type = request["type"] as? String ?? ""

        var reply: [String: Any] =
            forward(&request, launchIfDown: type != "ping")
            ?? ["ok": false, "error": "hydra is not running"]
        if reply["ok"] == nil { reply["ok"] = false }

        let response = NSExtensionItem()
        if #available(macOS 11.0, *) {
            response.userInfo = [SFExtensionMessageKey: reply]
        } else {
            response.userInfo = [extensionMessageKey: reply]
        }
        context.completeRequest(returningItems: [response])
    }

    private func appDir() -> URL {
        // Matches hydra-gui's model::app_dir: ~/.config/hydra on macOS.
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".config")
            .appendingPathComponent("hydra")
    }

    private func readIpc() -> (port: UInt16, token: String)? {
        guard
            let data = try? Data(contentsOf: appDir().appendingPathComponent("ipc.json")),
            let v = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let port = v["port"] as? Int,
            let token = v["token"] as? String
        else { return nil }
        return (UInt16(port), token)
    }

    private func forward(_ request: inout [String: Any], launchIfDown: Bool) -> [String: Any]? {
        if let r = roundTrip(&request) { return r }
        guard launchIfDown else { return nil }
        launchHydra()
        let deadline = Date().addingTimeInterval(20)
        while Date() < deadline {
            Thread.sleep(forTimeInterval: 0.3)
            if let r = roundTrip(&request) { return r }
        }
        return nil
    }

    private func launchHydra() {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/open")
        p.arguments = ["-ga", "Hydra Download Manager", "--args", "--minimized"]
        try? p.run()
        p.waitUntilExit()
    }

    /// One request/reply over a plain BSD socket; nil on any failure so the
    /// caller can launch the app and retry.
    private func roundTrip(_ request: inout [String: Any]) -> [String: Any]? {
        guard let ipc = readIpc() else { return nil }
        request["token"] = ipc.token
        guard var payload = try? JSONSerialization.data(withJSONObject: request) else {
            return nil
        }
        payload.append(0x0A)

        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return nil }
        defer { close(fd) }
        var tv = timeval(tv_sec: 5, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))

        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = ipc.port.bigEndian
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")
        let connected = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard connected == 0 else { return nil }

        let sent = payload.withUnsafeBytes { send(fd, $0.baseAddress, payload.count, 0) }
        guard sent == payload.count else { return nil }

        var line = Data()
        var byte: UInt8 = 0
        while line.count < 1 << 20 {
            let n = recv(fd, &byte, 1, 0)
            if n <= 0 { return nil }
            if byte == 0x0A { break }
            line.append(byte)
        }
        return (try? JSONSerialization.jsonObject(with: line)) as? [String: Any]
    }
}
