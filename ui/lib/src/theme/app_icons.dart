import 'package:flutter/widgets.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

/// The four sizes an icon may take: 16 beside a label or inline, 18 in a
/// button or menu, 20 in the rail and the bottom bar, 24 for an empty state
/// or a tile.
abstract final class AppIconSize {
  static const double inline = 16, button = 18, nav = 20, tile = 24;
}

/// The app's one icon family: Lucide (ISC). Named for what each glyph means
/// here, so a redraw is one line rather than a search through every page.
///
/// Weight follows size. Every constant is Lucide's 400 stroke, for 16-20 px,
/// where the 300 stroke thins to grey; the `…Large` ones are the 300 stroke,
/// for 24 px and up. They are separate constants rather than one weight
/// chosen at runtime, because a non-const [IconData] defeats the icon font's
/// tree shaking.
abstract final class AppIcons {
  // Destinations.
  static const IconData overview = LucideIcons.orbit400;
  static const IconData tunnels = LucideIcons.route400;
  static const IconData remote = LucideIcons.monitor400;
  static const IconData relay = LucideIcons.radioTower400;
  static const IconData logs = LucideIcons.scrollText400;
  static const IconData settings = LucideIcons.settings400;

  // The two ends of a tunnel, and what they connect.
  static const IconData share = LucideIcons.arrowUpFromLine400;
  static const IconData connect = LucideIcons.arrowDownToLine400;

  /// A path straight between two computers, past the relay.
  static const IconData direct = LucideIcons.zap400;

  /// Some sessions direct, some through the relay.
  static const IconData mixedPath = LucideIcons.split400;

  // How this computer reaches the relay: a stream per tunnel, or every
  // stream layered on one QUIC connection.
  static const IconData carrierTcp = LucideIcons.arrowRightLeft400;
  static const IconData carrierQuic = LucideIcons.layers400;
  static const IconData computer = LucideIcons.laptop400;
  static const IconData service = LucideIcons.server400;

  /// A tunnel's diagnostics, and the router a port mapping asks.
  static const IconData diagnostics = LucideIcons.stethoscope400;
  static const IconData router = LucideIcons.router400;

  // Actions.
  static const IconData add = LucideIcons.plus400;
  static const IconData close = LucideIcons.x400;
  static const IconData copy = LucideIcons.copy400;
  static const IconData edit = LucideIcons.pencil400;
  static const IconData delete = LucideIcons.trash2400;
  static const IconData start = LucideIcons.play400;
  static const IconData stop = LucideIcons.square400;
  static const IconData refresh = LucideIcons.refreshCw400;
  static const IconData search = LucideIcons.search400;
  static const IconData more = LucideIcons.ellipsis400;
  static const IconData export = LucideIcons.fileUp400;
  static const IconData import = LucideIcons.clipboardPaste400;

  /// The same glyph as [import], named by what it does there.
  static const IconData paste = LucideIcons.clipboardPaste400;
  static const IconData external = LucideIcons.externalLink400;
  static const IconData clear = LucideIcons.eraser400;
  static const IconData follow = LucideIcons.arrowDown400;

  // State.
  static const IconData check = LucideIcons.check400;
  static const IconData ok = LucideIcons.circleCheck400;
  static const IconData info = LucideIcons.info400;
  static const IconData warning = LucideIcons.triangleAlert400;
  static const IconData error = LucideIcons.circleAlert400;
  static const IconData online = LucideIcons.cloud400;
  static const IconData offline = LucideIcons.cloudOff400;
  static const IconData lock = LucideIcons.lock400;
  static const IconData key = LucideIcons.keyRound400;
  static const IconData link = LucideIcons.link400;
  static const IconData unlink = LucideIcons.unlink400;
  static const IconData show = LucideIcons.eye400;
  static const IconData hide = LucideIcons.eyeOff400;

  // Settings.
  static const IconData themeSystem = LucideIcons.sunMoon400;
  static const IconData themeLight = LucideIcons.sun400;
  static const IconData themeDark = LucideIcons.moon400;
  static const IconData star = LucideIcons.star400;
  static const IconData update = LucideIcons.circleFadingArrowUp400;
  static const IconData download = LucideIcons.download400;

  // Disclosure.
  static const IconData chevronDown = LucideIcons.chevronDown400;
  static const IconData chevronUp = LucideIcons.chevronUp400;
  static const IconData chevronRight = LucideIcons.chevronRight400Dir;
  static const IconData back = LucideIcons.arrowLeft400Dir;
  static const IconData forward = LucideIcons.arrowRight400Dir;

  // Remote desktop: sharing this computer, and viewing another.
  static const IconData screenShare = LucideIcons.screenShare400;
  static const IconData screenShareOff = LucideIcons.screenShareOff400;

  /// The device viewing this computer, whatever kind it is.
  static const IconData remoteDevices = LucideIcons.monitorSmartphone400;
  static const IconData viewOnly = LucideIcons.eye400;
  static const IconData control = LucideIcons.mousePointer2400;
  static const IconData keyboard = LucideIcons.keyboard400;
  static const IconData shortcuts = LucideIcons.command400;
  static const IconData rightClick = LucideIcons.mouse400;
  static const IconData trackpad = LucideIcons.touchpad400;
  static const IconData directTouch = LucideIcons.pointer400;
  static const IconData qrCode = LucideIcons.qrCode400;
  static const IconData scanQrCode = LucideIcons.scanQrCode400;
  static const IconData fullscreen = LucideIcons.maximize400;
  static const IconData exitFullscreen = LucideIcons.minimize400;
  static const IconData pin = LucideIcons.pin400;
  static const IconData unpin = LucideIcons.pinOff400;
  static const IconData disconnect = LucideIcons.power400;

  /// The protected helper service that reaches UAC and the lock screen.
  static const IconData helper = LucideIcons.shieldCheck400;

  /// The remote computer showing UAC or its lock screen.
  static const IconData secureDesktop = LucideIcons.lockKeyhole400;
  static const IconData quality = LucideIcons.gauge400;
  static const IconData phone = LucideIcons.smartphone400;

  /// Asked for and not granted yet: the viewer's Control while it waits.
  static const IconData pending = LucideIcons.loaderCircle400;

  // The 300 stroke, for 24 px and up.
  static const IconData relayLarge = LucideIcons.radioTower300;
  static const IconData remoteLarge = LucideIcons.monitor300;
  static const IconData scanQrCodeLarge = LucideIcons.scanQrCode300;
}
