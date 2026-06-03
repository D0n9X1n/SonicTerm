#!/usr/bin/env swift
// #549: returns CGWindowID for the frontmost normal-level window owned by PID.
// #589: --count <pid> prints window count; --bounds <pid> prints "x y w h".
import CoreGraphics
import Foundation

let args = CommandLine.arguments
func usage() -> Never {
    FileHandle.standardError.write("usage: cg-window-id.swift [--count|--bounds] <pid>\n".data(using: .utf8)!)
    exit(2)
}
var mode = "id", pidArg: String? = nil
if args.count == 2 { pidArg = args[1] }
else if args.count == 3 && (args[1] == "--count" || args[1] == "--bounds") {
    mode = String(args[1].dropFirst(2)); pidArg = args[2]
} else { usage() }
guard let pidStr = pidArg, let targetPid = Int32(pidStr) else { usage() }

let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
guard let infos = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: AnyObject]] else { exit(3) }
var matches: [[String: AnyObject]] = []
for info in infos {
    let pid = info[kCGWindowOwnerPID as String] as? Int32 ?? -1
    let layer = info[kCGWindowLayer as String] as? Int ?? -1
    if pid == targetPid && layer == 0 { matches.append(info) }
}
switch mode {
case "count":
    print(matches.count); exit(0)
case "bounds":
    guard let info = matches.first,
          let b = info[kCGWindowBounds as String] as? [String: CGFloat],
          let x = b["X"], let y = b["Y"], let w = b["Width"], let h = b["Height"] else { exit(1) }
    print("\(Int(x)) \(Int(y)) \(Int(w)) \(Int(h))"); exit(0)
default:
    guard let info = matches.first,
          let wid = info[kCGWindowNumber as String] as? UInt32 else { exit(1) }
    print(wid); exit(0)
}
