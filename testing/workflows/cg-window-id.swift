#!/usr/bin/env swift
// #549: returns CGWindowID for the frontmost normal-level window owned by the given PID.
// CoreGraphics window IDs are the only thing `screencapture -l` accepts; AppleScript
// `id of window 1` does not return one for non-AppleScript-aware apps like winit-based binaries.
import CoreGraphics
import Foundation

guard CommandLine.arguments.count >= 2, let targetPid = Int32(CommandLine.arguments[1]) else {
    FileHandle.standardError.write("usage: cg-window-id.swift <pid>\n".data(using: .utf8)!)
    exit(2)
}

let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
guard let infos = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: AnyObject]] else {
    exit(3)
}

for info in infos {
    let pid = info[kCGWindowOwnerPID as String] as? Int32 ?? -1
    let layer = info[kCGWindowLayer as String] as? Int ?? -1
    if pid == targetPid && layer == 0 {
        if let wid = info[kCGWindowNumber as String] as? UInt32 {
            print(wid)
            exit(0)
        }
    }
}
exit(1) // no matching window yet
