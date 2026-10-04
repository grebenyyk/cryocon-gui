#!/usr/bin/env swift
// Generate the cryocon-gui app icon: a snowflake over a temperature-trace
// arc, in the app's chart colors (BLUE #2a78d6 / ORANGE #eb6834).
//
//   swift scripts/make_icon.swift            # assets/icon-1024.png + AppIcon.icns
//   swift scripts/make_icon.swift --sweep    # /tmp/cryocon-icon-sweep.png — labeled grid
//                                            # of layout variants to choose from
//
// Pure CoreGraphics + CoreText, no AppKit.

import Foundation
import CoreGraphics
import CoreText
import ImageIO

let W: CGFloat = 1024                       // design space (y is up)
let cs = CGColorSpace(name: CGColorSpace.sRGB)!

// Vertical layout knobs (the only things the sweep varies).
struct Layout {
    var flakeR: CGFloat   // snowflake arm radius
    var flakeY: CGFloat   // snowflake center height
    var traceY: CGFloat   // height of the trace arc's ends
}

let standard = Layout(flakeR: 255, flakeY: 560, traceY: 290)   // A3 from the sweep

func rgb(_ r: CGFloat, _ g: CGFloat, _ b: CGFloat, _ a: CGFloat = 1) -> CGColor {
    CGColor(colorSpace: cs, components: [r, g, b, a])!
}

func gradient(_ colors: [CGColor], _ locs: [CGFloat]) -> CGGradient {
    CGGradient(colorsSpace: cs, colors: colors as CFArray, locations: locs)!
}

func newCtx(_ w: Int, _ h: Int) -> CGContext {
    CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
              space: cs, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
}

// One snowflake arm + hex core, pointing up, then 6 rotations around `center`.
func drawSnowflake(_ c: CGContext, center: CGPoint, R: CGFloat, bold: CGFloat) {
    c.translateBy(x: center.x, y: center.y)
    let hex = CGMutablePath()
    var p = CGPoint(x: 0, y: 48)
    hex.move(to: p)
    for a in stride(from: 60.0, through: 300, by: 60) {
        let r = a * .pi / 180
        p = CGPoint(x: 48 * sin(r), y: 48 * cos(r))
        hex.addLine(to: p)
    }
    hex.closeSubpath()
    c.addPath(hex)
    c.fillPath()

    c.setLineCap(.round)
    for k in 0..<6 {
        c.saveGState()
        c.rotate(by: CGFloat(k) * .pi / 3)
        c.setLineWidth(34 * bold)
        c.move(to: CGPoint(x: 0, y: 42))
        c.addLine(to: CGPoint(x: 0, y: R))
        c.strokePath()
        c.setLineWidth(22 * bold)
        for s in [CGFloat(-1), CGFloat(1)] {
            c.move(to: CGPoint(x: 0, y: R * 0.56))
            c.addLine(to: CGPoint(x: s * R * 0.26, y: R * 0.80))
            c.move(to: CGPoint(x: 0, y: R * 0.30))
            c.addLine(to: CGPoint(x: s * R * 0.19, y: R * 0.47))
        }
        c.strokePath()
        c.restoreGState()
    }
}

// The temperature trace as a true circular arc dipping by `dip` from `a` to
// `b`: cool down to the base temperature, then warm back up (and a happy
// icon, not a sad or smirking one).
func drawTrace(_ c: CGContext, a: CGPoint, b: CGPoint, dip: CGFloat, bold: CGFloat) {
    c.setLineCap(.round)
    c.setLineWidth(23 * bold)
    let half = (b.x - a.x) / 2
    let r = (dip * dip + half * half) / (2 * dip)   // circle through a, b
    let center = CGPoint(x: (a.x + b.x) / 2, y: a.y + r - dip)
    c.addArc(center: center, radius: r,
             startAngle: atan2(a.y - center.y, a.x - center.x),
             endAngle: atan2(b.y - center.y, b.x - center.x),
             clockwise: false)
    c.strokePath()
}

// Tint a white-on-transparent shape layer with a gradient (alpha preserved).
func tinted(_ shape: (CGContext) -> Void, size: Int, colors: [CGColor]) -> CGImage {
    let c = newCtx(size, size)
    c.scaleBy(x: CGFloat(size) / W, y: CGFloat(size) / W)
    c.setFillColor(rgb(1, 1, 1))
    c.setStrokeColor(rgb(1, 1, 1))
    shape(c)
    c.setBlendMode(.sourceAtop)
    c.drawLinearGradient(gradient(colors, [0, 1]),
                         start: CGPoint(x: 180, y: 980), end: CGPoint(x: 700, y: 80),
                         options: [])
    return c.makeImage()!
}

func drawIcon(_ c: CGContext, px: Int, bold: CGFloat, L: Layout) {
    let iconRect = CGRect(x: 100, y: 100, width: 824, height: 824)
    c.addPath(CGPath(roundedRect: iconRect, cornerWidth: 185, cornerHeight: 185, transform: nil))
    c.clip()

    // night-cryo background
    c.drawLinearGradient(
        gradient([rgb(0.10, 0.22, 0.36), rgb(0.065, 0.155, 0.27), rgb(0.03, 0.08, 0.15)],
                 [0, 0.55, 1]),
        start: CGPoint(x: 512, y: 1024), end: CGPoint(x: 512, y: 0), options: [])

    // cold glow behind the flake
    let flakeCenter = CGPoint(x: 512, y: L.flakeY)
    c.drawRadialGradient(
        gradient([rgb(0.42, 0.68, 1.0, 0.30), rgb(0.42, 0.68, 1.0, 0)], [0, 1]),
        startCenter: flakeCenter, startRadius: 0,
        endCenter: flakeCenter, endRadius: 400, options: [])

    // snowflake: glow pass, then crisp pass
    let flake = tinted({ drawSnowflake($0, center: flakeCenter, R: L.flakeR, bold: bold) },
                       size: px, colors: [rgb(0.93, 0.97, 1.0), rgb(0.36, 0.62, 0.92)])
    let flakeRect = CGRect(x: 0, y: 0, width: W, height: W)
    c.saveGState()
    c.setShadow(offset: .zero, blur: 42, color: rgb(0.45, 0.75, 1.0, 0.6))
    c.draw(flake, in: flakeRect)
    c.restoreGState()
    c.draw(flake, in: flakeRect)

    // temperature trace with a live setpoint dot at its leading end
    let traceA = CGPoint(x: 304, y: L.traceY)
    let traceB = CGPoint(x: 740, y: L.traceY)
    let trace = tinted({ drawTrace($0, a: traceA, b: traceB, dip: 70, bold: bold) },
                       size: px, colors: [rgb(1.0, 0.60, 0.31), rgb(0.86, 0.34, 0.12)])
    c.draw(trace, in: flakeRect)
    let dot = tinted({
        $0.fillEllipse(in: CGRect(x: traceB.x - 24, y: traceB.y - 24, width: 48, height: 48))
    }, size: px, colors: [rgb(1.0, 0.78, 0.55), rgb(1.0, 0.55, 0.25)])
    c.draw(dot, in: flakeRect)

    // top sheen + hairline edge
    c.drawLinearGradient(gradient([rgb(1, 1, 1, 0.09), rgb(1, 1, 1, 0)], [0, 1]),
                         start: CGPoint(x: 512, y: 1024), end: CGPoint(x: 512, y: 560),
                         options: [])
    c.setStrokeColor(rgb(1, 1, 1, 0.10))
    c.setLineWidth(2.5)
    c.addPath(CGPath(roundedRect: iconRect.insetBy(dx: 1.25, dy: 1.25),
                     cornerWidth: 184, cornerHeight: 184, transform: nil))
    c.strokePath()
}

func write(_ img: CGImage, _ path: String) {
    let dest = CGImageDestinationCreateWithURL(URL(fileURLWithPath: path) as CFURL,
                                               "public.png" as CFString, 1, nil)!
    CGImageDestinationAddImage(dest, img, nil)
    CGImageDestinationFinalize(dest)
}

func render(_ px: Int, _ L: Layout = standard) -> CGImage {
    let bold: CGFloat = px >= 128 ? 1.0 : (px >= 48 ? 1.15 : 1.35)
    let c = newCtx(px, px)
    c.scaleBy(x: CGFloat(px) / W, y: CGFloat(px) / W)
    drawIcon(c, px: px, bold: bold, L: L)
    return c.makeImage()!
}

func drawLabel(_ c: CGContext, _ text: String, at p: CGPoint) {
    let font = CTFontCreateWithName("Menlo" as CFString, 30, nil)
    let attr = NSAttributedString(string: text, attributes: [
        NSAttributedString.Key(kCTFontAttributeName as String): font,
        NSAttributedString.Key(kCTForegroundColorAttributeName as String):
            CGColor(gray: 0.15, alpha: 1),
    ])
    let line = CTLineCreateWithAttributedString(attr)
    let width = CGFloat(CTLineGetTypographicBounds(line, nil, nil, nil))
    c.textPosition = CGPoint(x: p.x - width / 2, y: p.y)
    CTLineDraw(line, c)
}

func sweep() {
    // rows: flake height (higher / lower) — columns: flake size
    let variants: [(String, Layout)] = [
        ("A1  R210 y560  (current)", Layout(flakeR: 210, flakeY: 560, traceY: 322)),
        ("A2  R235 y560",            Layout(flakeR: 235, flakeY: 560, traceY: 300)),
        ("A3  R255 y560",            Layout(flakeR: 255, flakeY: 560, traceY: 290)),
        ("B1  R210 y520",            Layout(flakeR: 210, flakeY: 520, traceY: 310)),
        ("B2  R235 y520",            Layout(flakeR: 235, flakeY: 520, traceY: 290)),
        ("B3  R255 y520",            Layout(flakeR: 255, flakeY: 520, traceY: 280)),
    ]
    let cell = 512, labelH = 56, gap = 36
    let cols = 3, rows = 2
    let cw = cell * cols + gap * (cols + 1)
    let ch = (cell + labelH) * rows + gap * (rows + 1)
    let c = newCtx(cw, ch)
    c.setFillColor(rgb(0.78, 0.78, 0.80))
    c.fill(CGRect(x: 0, y: 0, width: cw, height: ch))
    for (i, (name, L)) in variants.enumerated() {
        let col = i % cols, row = i / cols
        let x = gap + col * (cell + gap)
        let y = ch - gap - (row + 1) * (cell + labelH) - row * gap
        c.draw(render(cell, L), in: CGRect(x: x, y: y, width: cell, height: cell))
        drawLabel(c, name, at: CGPoint(x: x + cell / 2, y: y + 12))
    }
    write(c.makeImage()!, "/tmp/cryocon-icon-sweep.png")
    print("wrote /tmp/cryocon-icon-sweep.png")
}

if CommandLine.arguments.dropFirst().first == "--sweep" {
    sweep()
} else {
    let root = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
    let assets = root.appendingPathComponent("assets")
    let iconset = assets.appendingPathComponent("AppIcon.iconset")
    try? FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)

    write(render(1024), assets.appendingPathComponent("icon-1024.png").path)

    // the full iconset iconutil accepts
    let sizes: [(Int, String)] = [
        (16, "icon_16x16.png"), (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"), (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"), (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"), (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"), (1024, "icon_512x512@2x.png"),
    ]
    for (px, name) in sizes {
        write(render(px), iconset.appendingPathComponent(name).path)
    }

    let proc = Process()
    proc.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
    proc.arguments = ["-c", "icns", iconset.path,
                      "-o", assets.appendingPathComponent("AppIcon.icns").path]
    try proc.run()
    proc.waitUntilExit()
    if proc.terminationStatus != 0 { exit(proc.terminationStatus) }
    try? FileManager.default.removeItem(at: iconset)
    print("wrote assets/icon-1024.png and assets/AppIcon.icns")
}
