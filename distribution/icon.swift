import AppKit
import Foundation
let output = CommandLine.arguments[1]
let terminal = CommandLine.arguments[2] == "terminal"
let size = 1024
let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
NSColor(calibratedRed: 0.08, green: 0.13, blue: 0.21, alpha: 1).setFill()
NSBezierPath(roundedRect: NSRect(x: 64, y: 64, width: 896, height: 896), xRadius: 200, yRadius: 200).fill()
NSColor(calibratedRed: terminal ? 0.35 : 0.42, green: 0.85, blue: terminal ? 0.67 : 1, alpha: 1).setStroke()
let path = NSBezierPath()
path.lineWidth = 70
path.lineCapStyle = .round
if terminal {
    path.move(to: NSPoint(x: 275, y: 680)); path.line(to: NSPoint(x: 455, y: 515)); path.line(to: NSPoint(x: 275, y: 350))
    path.move(to: NSPoint(x: 545, y: 350)); path.line(to: NSPoint(x: 750, y: 350))
} else {
    for y in [350, 510, 670] {
        path.move(to: NSPoint(x: 290, y: y)); path.line(to: NSPoint(x: y == 350 ? 540 : 735, y: y))
    }
}
path.stroke()
NSGraphicsContext.restoreGraphicsState()
try bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: output))
