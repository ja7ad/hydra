// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

// Run with: swift scripts/macos-login-e2e.swift target/debug/hydra-gui [--tray-only]
import AppKit
import CoreGraphics

func require(_ condition: @autoclosure () -> Bool, _ message: String) {
    if !condition() { fatalError(message) }
}

var monitorStartup = false
var startupSawWindow = false

func waitUntil(_ condition: () -> Bool) -> Bool {
    let deadline = Date().addingTimeInterval(15)
    while Date() < deadline {
        if monitorStartup {
            startupSawWindow = startupSawWindow || NSRunningApplication.runningApplications(
                withBundleIdentifier: "io.github.ja7ad.hydra.login-e2e"
            ).contains { visibleWindows($0.processIdentifier) > 0 }
        }
        if condition() { return true }
        RunLoop.current.run(until: Date().addingTimeInterval(0.01))
    }
    return condition()
}

func visibleWindows(_ pid: pid_t) -> Int {
    let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
    return windows.filter {
        ($0[kCGWindowOwnerPID as String] as? Int) == Int(pid)
            && ($0[kCGWindowLayer as String] as? Int) == 0
    }.count
}

let fm = FileManager.default
require(CommandLine.arguments.count == 2 || (CommandLine.arguments.count == 3 && CommandLine.arguments[2] == "--tray-only"), "Supply the hydra-gui binary path and optional --tray-only")
let trayOnly = CommandLine.arguments.count == 3
let binary = URL(fileURLWithPath: CommandLine.arguments[1]).standardizedFileURL
let root = fm.temporaryDirectory.appendingPathComponent("hydra-login-e2e-\(UUID().uuidString)")
let bundle = root.appendingPathComponent("Hydra Login E2E.app")
let macos = bundle.appendingPathComponent("Contents/MacOS")
try fm.createDirectory(at: macos, withIntermediateDirectories: true)
try fm.copyItem(at: binary, to: macos.appendingPathComponent("hydra-gui"))
let info: [String: Any] = [
    "CFBundleExecutable": "hydra-gui",
    "CFBundleIdentifier": "io.github.ja7ad.hydra.login-e2e",
    "CFBundleName": "Hydra Login E2E",
    "CFBundlePackageType": "APPL",
    "NSHighResolutionCapable": true,
]
try PropertyListSerialization.data(fromPropertyList: info, format: .xml, options: 0)
    .write(to: bundle.appendingPathComponent("Contents/Info.plist"))
var applications: [NSRunningApplication] = []
defer {
    for app in applications where !app.isTerminated {
        app.terminate()
        if !waitUntil({ app.isTerminated }) { app.forceTerminate() }
    }
    try? fm.removeItem(at: root)
}

func launch(_ profile: URL, login: Bool = false, minimized: Bool = false, newInstance: Bool = true) throws -> NSRunningApplication {
    let configuration = NSWorkspace.OpenConfiguration()
    configuration.createsNewApplicationInstance = newInstance
    if let coverage = ProcessInfo.processInfo.environment["LLVM_PROFILE_FILE"] {
        configuration.environment = ["LLVM_PROFILE_FILE": coverage]
    }
    configuration.arguments = ["--config", profile.path] + (minimized ? ["--minimized"] : [])
    if login {
        let event = NSAppleEventDescriptor(eventClass: 0x61657674, eventID: 0x6f617070,
            targetDescriptor: nil, returnID: -1, transactionID: 0)
        event.setParam(NSAppleEventDescriptor(enumCode: 0x6c676974), forKeyword: 0x70726f70)
        configuration.appleEvent = event
    }
    var application: NSRunningApplication?
    var failure: Error?
    NSWorkspace.shared.openApplication(at: bundle, configuration: configuration) { app, error in
        application = app
        failure = error
    }
    require(waitUntil { application != nil || failure != nil }, "Launch timed out")
    if let failure { throw failure }
    let app = application!
    applications.append(app)
    return app
}

func profile(_ name: String, tray: Bool) throws -> URL {
    let profile = root.appendingPathComponent(name)
    try fm.createDirectory(at: profile, withIntermediateDirectories: true)
    try "[settings]\nstart_in_tray = \(tray)\ncheck_updates_on_startup = false\nmonitor_clipboard = false\n"
        .write(to: profile.appendingPathComponent("config.toml"), atomically: true, encoding: .utf8)
    return profile
}

func ready(_ profile: URL) -> Bool {
    fm.fileExists(atPath: profile.appendingPathComponent("ipc.json").path)
}

func log(_ profile: URL) -> String {
    (try? String(contentsOf: profile.appendingPathComponent("logs/gui.log"), encoding: .utf8)) ?? ""
}

func stop(_ app: NSRunningApplication) {
    app.terminate()
    require(waitUntil { app.isTerminated }, "App did not terminate")
}

for (name, login, tray, explicit, hidden) in [
    ("manual-tray-enabled", false, true, false, false),
    ("login-tray-enabled", true, true, false, true),
    ("login-tray-disabled", true, false, false, false),
    ("explicit-minimized", false, false, true, true),
] {
    if trayOnly && !hidden { continue }
    let data = try profile(name, tray: tray)
    startupSawWindow = false
    monitorStartup = hidden
    let app = try launch(data, login: login, minimized: explicit)
    require(waitUntil { ready(data) }, "\(name): IPC never became ready")
    if hidden {
        require(waitUntil { log(data).contains("started minimized to tray") }, "\(name): tray startup missing")
        let observedUntil = Date().addingTimeInterval(1)
        _ = waitUntil { Date() >= observedUntil }
        monitorStartup = false
        require(!startupSawWindow, "\(name): a window flashed during tray startup")
        require(visibleWindows(app.processIdentifier) == 0, "\(name): unexpectedly opened a window")
        let duplicate = try launch(data, login: true, minimized: explicit)
        require(waitUntil { duplicate.isTerminated }, "\(name): duplicate instance did not exit")
        require(visibleWindows(app.processIdentifier) == 0, "\(name): login duplicate revealed the window")
        if trayOnly {
            print("PASS: \(name), no startup window flash, quiet duplicate login")
            stop(app)
            continue
        }
        let manual = try launch(data)
        require(waitUntil { manual.isTerminated }, "\(name): manual duplicate did not exit")
        require(waitUntil { visibleWindows(app.processIdentifier) > 0 }, "\(name): manual launch did not reveal existing instance")
        print("PASS: \(name), quiet duplicate login, manual reopen")
    } else {
        require(waitUntil { visibleWindows(app.processIdentifier) > 0 }, "\(name): main window missing")
        print("PASS: \(name)")
    }
    stop(app)
}
if !trayOnly {
    let dockProfile = try profile("dock-reopen", tray: true)
    let dockApp = try launch(dockProfile, login: true)
    require(waitUntil { log(dockProfile).contains("started minimized to tray") }, "Dock test: tray startup missing")
    require(visibleWindows(dockApp.processIdentifier) == 0, "Dock test: unexpected startup window")
    let reopened = try launch(dockProfile, newInstance: false)
    require(reopened.processIdentifier == dockApp.processIdentifier, "Dock test: launch did not reuse the app")
    require(waitUntil { visibleWindows(dockApp.processIdentifier) > 0 }, "Dock reopen did not show the window")
    stop(dockApp)
    print("PASS: Dock reopen reveals the tray instance")
}
print("PASS: macOS launch-event E2E; no login registration was changed")
