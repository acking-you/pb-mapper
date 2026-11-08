package com.pb_mapper.ui

import android.app.Activity
import android.os.Build
import android.view.Display

/**
 * The activity window's display mode while remote video shows: the fastest
 * refresh at the display's current resolution, so a picture released late in
 * a frame still has a vsync to land on. The window's own preference returns
 * when the last holder lets go. Main thread only.
 */
class DisplayRefresh(private val activity: Activity) {
    private var holders = 0
    private var previous = 0
    private var requested: Display.Mode? = null

    fun hold() {
        if (holders++ > 0) return
        val display = display() ?: return
        val current = display.mode
        val fastest = display.supportedModes
            .filter { it.physicalWidth == current.physicalWidth && it.physicalHeight == current.physicalHeight }
            .maxByOrNull { it.refreshRate } ?: return
        val attributes = activity.window.attributes
        previous = attributes.preferredDisplayModeId
        attributes.preferredDisplayModeId = fastest.modeId
        activity.window.attributes = attributes
        requested = fastest
    }

    fun release() {
        if (holders == 0 || --holders > 0 || requested == null) return
        requested = null
        val attributes = activity.window.attributes
        attributes.preferredDisplayModeId = previous
        activity.window.attributes = attributes
    }

    /** The refresh asked for while video shows, otherwise the display's current one. */
    val refreshHz: Double get() = (requested ?: display()?.mode)?.refreshRate?.toDouble() ?: 0.0

    @Suppress("DEPRECATION")
    private fun display(): Display? =
        if (Build.VERSION.SDK_INT >= 30) activity.display else activity.windowManager.defaultDisplay
}
