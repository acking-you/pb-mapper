import 'package:flutter/widgets.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

/// The app's one icon family: Lucide (ISC), at its 300 stroke, which sits
/// with Google Sans Flex's weight. Named for what each glyph means here, so a
/// redraw is one line rather than a search through every page.
abstract final class AppIcons {
  // Destinations.
  static const IconData overview = LucideIcons.orbit300;
  static const IconData tunnels = LucideIcons.cable300;
  static const IconData logs = LucideIcons.scrollText300;
  static const IconData settings = LucideIcons.settings300;

  // The two ends of a tunnel, and what they connect.
  static const IconData share = LucideIcons.arrowUpFromLine300;
  static const IconData connect = LucideIcons.arrowDownToLine300;
  static const IconData relay = LucideIcons.radioTower300;
  static const IconData computer = LucideIcons.laptop300;
  static const IconData service = LucideIcons.server300;

  // Actions.
  static const IconData add = LucideIcons.plus300;
  static const IconData close = LucideIcons.x300;
  static const IconData copy = LucideIcons.copy300;
  static const IconData edit = LucideIcons.pencil300;
  static const IconData delete = LucideIcons.trash2300;
  static const IconData start = LucideIcons.play300;
  static const IconData stop = LucideIcons.square300;
  static const IconData refresh = LucideIcons.refreshCw300;
  static const IconData search = LucideIcons.search300;
  static const IconData more = LucideIcons.ellipsis300;
  static const IconData export = LucideIcons.fileUp300;
  static const IconData import = LucideIcons.clipboardPaste300;
  static const IconData external = LucideIcons.externalLink300;
  static const IconData clear = LucideIcons.eraser300;
  static const IconData follow = LucideIcons.arrowDown300;
  static const IconData list = LucideIcons.list300;

  // State.
  static const IconData check = LucideIcons.check300;
  static const IconData ok = LucideIcons.circleCheck300;
  static const IconData info = LucideIcons.info300;
  static const IconData warning = LucideIcons.triangleAlert300;
  static const IconData error = LucideIcons.circleAlert300;
  static const IconData online = LucideIcons.wifi300;
  static const IconData offline = LucideIcons.wifiOff300;
  static const IconData lock = LucideIcons.lock300;
  static const IconData key = LucideIcons.keyRound300;
  static const IconData link = LucideIcons.link300;
  static const IconData unlink = LucideIcons.unlink300;
  static const IconData show = LucideIcons.eye300;
  static const IconData hide = LucideIcons.eyeOff300;

  // Settings.
  static const IconData themeSystem = LucideIcons.sunMoon300;
  static const IconData themeLight = LucideIcons.sun300;
  static const IconData themeDark = LucideIcons.moon300;
  static const IconData language = LucideIcons.languages300;
  static const IconData star = LucideIcons.star300;
  static const IconData update = LucideIcons.circleFadingArrowUp300;
  static const IconData download = LucideIcons.download300;

  // Disclosure.
  static const IconData chevronDown = LucideIcons.chevronDown300;
  static const IconData chevronRight = LucideIcons.chevronRight300Dir;
  static const IconData back = LucideIcons.arrowLeft300Dir;
  static const IconData forward = LucideIcons.arrowRight300Dir;
}
