Screenshotter
=============

A tool to take screenshots and organize them based on the name of the application in the screenshot. Supports full desktop screenshots, taking screenshots of windows, or selecting regions.

# Requirements

Wayland version:
* Sway or Hyprland
* Works as a replacement for common `swayprop` and `hyprprop` scripts, handling more edge cases.
* Does *not* require slurp or grim.
* As of 2026-05 the Wayland ecosystem is not in a a great state.

Terminal emulators or other multiplexers that have multiple windows with the same process and children are not well supported on Wayland.

X11 version:
* Go 1.11 or newer
* ImageMagick 6
* [slop](https://github.com/naelstrof/slop)
* [maim](https://github.com/naelstrof/maim)
    * Requires a version with support for BMP images.
    * As of the time of this writing the most recent tagged release does not support BMP.
* An X11 based desktop environment
    * xdotool
* Any terminal emulator except gnome-terminal
    * Screenshots will work but it's impossible to correctly determine which application is running in a window.
    * Other libvte based terminals are fine.

# Installation

The Go/X11 and Wayland/Rust versions use different configuration options, camelCase for go and snake_case for Rust. The configurations for both versions can exist in the same file to run both at once, but rename the X11 overrides array to OverrideX.

## Wayland (Sway/Hyprland)

`cargo install https://github.com/awused/screenshotter --locked`

Fill in [screenshotter.way.toml](screenshotter.way.toml) and copy it to $HOME/.screenshotter.toml or $HOME/.config/screenshotter/screenshotter.toml.


## X11
`go get -u github.com/awused/screenshotter`

Fill in [screenshotter.x.toml](screenshotter.x.toml) and copy it to $HOME/.screenshotter.toml or $HOME/.config/screenshotter/screenshotter.toml.

# Usage

Take a screenshot with `screenshotter {mode}` where mode is `desktop`, `window` (for the active window), `region` (to select a window or rectangular region), `name`, or `prop`.

The `name` mode does not take a screenshot and can be used to ensure that your configuration is working properly without polluting your screenshots directory.

The `prop` mode outputs json on stdout and can be used as a "swayprop" or "hyprprop" command. It does a better job at selecting the visible portion of the active window provided my fork of slurp is also used.

## Wayland Keyboard Controls

* `Escape/Q` - Exit
* `P/Left click` - Select the current highlighted window
* `Left click and hold` - Drag to select a region
* `Backspace/Right Click` - Cancel a drag
* `Shift+Left Click` - Start a drag without requiring left mouse to be held down
  * Holding `Shift` while releasing the left mouse button will not finish the selection
* `Enter/Space/Numpad 5` - Start or stop dragging
* `Arrow Keys/Numpad` - Move the mouse one pixel at a time

# Configuration

Most of the configuration in screenshotter.toml is straightforward but overrides and ignored parents can get complicated.

Use `IgnoredParents`/`ignored_parents` to cover the cases where you've started vim from zsh running in an xfce4-terminal window. Without any configuration the application name will be "xfce4-terminal", which is probably not what you want. By ignoring xfce4-terminal and zsh the tool will correctly detect "vim" as the application name.

Any program specified with `Callback`/`callback` will be run after the screenshot has been taken. The filename will be supplied as the first argument and the same environment variables as for delegates (see below) will be set.

For wayland you can speciy the positions of monitors in real pixels in `monitor_positions` to allow for unscaled outputs in some modes to be better positioned. There is no way to get perfect results across monitors with different scales.

## X11

When running terminals that share processes between windows (xfce4-terminal, gnome-terminal, etc) CheckWindowID must be set to true.

MouseKeys will turn [Mouse Keys](https://en.wikipedia.org/wiki/Mouse_keys) on and off when selecting a region if it isn't already enabled.

## Overrides

Use `Overrides`/`overrides` when you want to manipulate how the directories are named for an application. In the simple cases you might want "chromium-browser" to instead be "chromium" or "vimx" to be "vim." In the more complicated cases you can use regular expressions and matching groups to change "python my-application.py" into  "my-application." Use the `-debug` flag or the `name` command to assist with editing these.

Use `Yearly`/`yearly` and `Monthly`/`monthly` to configure separate subfolders.

### Delegates

Specify a program using `Delegate`/`delegate` inside an `Override` block to delegate directory naming to another program or script for a particular program.

A delegate that exits with a non-zero status will be ignored and the next matching override will be attempted.

The delegate program will be executed with several environment variables set:

Environment Variable | Explanation
-------------------- | ----------
SCREENSHOTTER_MODE | The mode used when calling screenshotter.
SCREENSHOTTER_NAME | The name screenshotter determined for the application. Based on the process, if found, otherwise it is determined by splitting the `SCREENSHOTTER_CLASS` on periods and taking the last segment (`org.place.Application` becomes `application`). When set for callbacks, this will be a relative directory and could include multiple path segments from earlier override scripts, but will not include the year or month subdirectories.
SCREENSHOTTER_CLASS | Wayland only. The full APP_ID, class or WM_CLASS (xwayland) from the window. May not be present.
SCREENSHOTTER_DIR | The absolute directory determined for the application. Equivalent to screenshot_dir from the config + `/$SCREENSHOTTER_NAME`. Does not include year/month subdirectories.
SCREENSHOTTER_WM_NAME | The title of the window from the window manager. May not be present.
SCREENSHOTTER_WINDOWID | X11 only. The Window ID of the Window used to determine the active process.
SCREENSHOTTER_WINDOW_ID | Wayland only. The ID of the window used to determine the active process, which is the stableId for Hyprland.
SCREENSHOTTER_PID | The PID of the process that matched this Override. May not be present.
SCREENSHOTTER_MOUSEX | X11 only. The X coordinate of the cursor when the screenshot was taken.
SCREENSHOTTER_MOUSEY | X11 only. The Y coordinate of the cursor when the screenshot was taken.
SCREENSHOTTER_GEOMETRY | The geometry string of the selected window or region.
SCREENSHOTTER_WINDOW_PID | Wayland only. The PID of the process from the window. `SCREENSHOTTER_PID`, if different, is a descendant of this process.


The mouse coordinates may not be meaningful in `window` or `desktop` modes.
On X11 the format of the geometry will be: [{WIDTH}][x{HEIGHT}][{+-}{XOFF}[{+-}{YOFF}]] (e.g. "1436x879+6720+2160").
On Wayland the format will be: XOFF,YOFF WIDTHxHEIGHT (e.g. "6720,2160 1436x879")

<!-- TODO Add initial mouse coordinates on mousedown for region mode -->

# Limitations

The selection logic for determining which application is running in a window looks for the most recently spawned child process, which is not guaranteed to be the visible application in a terminal or shell. Delegates can be used solve this problem in some cases.

If selecting a region spanning multiple visible windows the application will be based on the window under the mouse cursor (x11) or center of the region (wayland) when the user ends their selection.

# Credit

* [slurp](https://github.com/emersion/slurp) Used as a reference for some parts of the Wayland interface.
