// Draws the vdisplay app icon (1024x1024 PNG), flat and two-tone: a navy
// monitor on a light tile whose left half is lit in coral, the part of the
// screen a virtual display is shown in.
// Run by `make icon`: swift packaging/make-icon.swift <out.png>

import AppKit

let size = 1024
guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write("usage: make-icon.swift <out.png>\n".data(using: .utf8)!)
    exit(2)
}
let outPath = CommandLine.arguments[1]

func rgb(_ hex: UInt32) -> NSColor {
    NSColor(
        srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
        green: CGFloat((hex >> 8) & 0xFF) / 255,
        blue: CGFloat(hex & 0xFF) / 255,
        alpha: 1)
}

func fill(_ hex: UInt32, _ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat, radius: CGFloat) {
    rgb(hex).setFill()
    NSBezierPath(roundedRect: CGRect(x: x, y: y, width: w, height: h), xRadius: radius, yRadius: radius)
        .fill()
}

let tile: UInt32 = 0xF4F5F8
let navy: UInt32 = 0x1E2A44
let coral: UInt32 = 0xFF6A4D
let dim: UInt32 = 0x3A4A6B

guard let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
    let ctx = NSGraphicsContext(bitmapImageRep: rep)
else {
    FileHandle.standardError.write("could not create the bitmap\n".data(using: .utf8)!)
    exit(1)
}
NSGraphicsContext.current = ctx

// The macOS icon grid's 824 pt rounded square (origin bottom-left).
fill(tile, 100, 100, 824, 824, radius: 186)

// Monitor: bezel, then the screen's two halves.
let bezel = CGRect(x: 222, y: 340, width: 580, height: 380)
let inset: CGFloat = 30
let gap: CGFloat = 16
let half = (bezel.width - 2 * inset - gap) / 2
fill(navy, bezel.minX, bezel.minY, bezel.width, bezel.height, radius: 48)
fill(coral, bezel.minX + inset, bezel.minY + inset, half, bezel.height - 2 * inset, radius: 20)
fill(dim, bezel.minX + inset + half + gap, bezel.minY + inset, half, bezel.height - 2 * inset, radius: 20)

// Stand.
fill(navy, 472, 236, 80, 110, radius: 0)
fill(navy, 372, 214, 280, 44, radius: 22)

NSGraphicsContext.current = nil
guard let png = rep.representation(using: .png, properties: [:]) else {
    FileHandle.standardError.write("could not encode the PNG\n".data(using: .utf8)!)
    exit(1)
}
do {
    try png.write(to: URL(fileURLWithPath: outPath))
} catch {
    FileHandle.standardError.write("could not write \(outPath): \(error)\n".data(using: .utf8)!)
    exit(1)
}
