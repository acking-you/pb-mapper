import 'package:flutter/material.dart';

getThemeChangeButton(VoidCallback onToggleTheme, BuildContext context) =>
    Padding(
      padding: const EdgeInsets.only(right: 12.0),
      child: IconButton(
        icon: Icon(
          Theme.of(context).brightness == Brightness.dark
              ? Icons.light_mode
              : Icons.dark_mode,
        ),
        onPressed: onToggleTheme,
      ),
    );
