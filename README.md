Screenshotter
=============

A tool to take screenshots and organize them based on the name of the application in the screenshot. Supports full desktop screenshots, taking screenshots of windows, or selecting regions.

# Requirements

Wayland/Sway version
* Sway
* [slurp](https://github.com/emersion/slurp)
* [grim](https://gitlab.freedesktop.org/emersion/grim)

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

The Go/X11 and Wayland/Sway/Rust versions use different configuration options. The configurations for both versions can exist in the same file to run both at once.

## Wayland/Sway

`cargo install https://github.com/awused/screenshotter --locked`

Fill in [screenshotter.sway.toml](screenshotter.sway.toml) and copy it to $HOME/.screenshotter.toml or $HOME/.config/screenshotter/screenshotter.toml.


## X11
`go get -u github.com/awused/screenshotter`

Fill in [screenshotter.x.toml](screenshotter.x.toml) and copy it to $HOME/.screenshotter.toml or $HOME/.config/screenshotter/screenshotter.toml.

# Usage

Take a screenshot with `screenshotter {mode}` where mode is desktop, window (for the active window), region (to select a window or rectangular region), or name.

The `name` mode does not take a screenshot and can be used to ensure that your configuration is working properly without polluting your screenshots directory.

# Configuration

Most of the configuration in screenshotter.toml is straightforward but overrides and ignored parents can get complicated.

Use `IgnoredParents`/`ignored_parents` to cover the cases where you've started vim from zsh running in an xfce4-terminal window. Without any configuration the application name will be "xfce4-terminal", which is probably not what you want. By ignoring xfce4-terminal and zsh the tool will correctly detect "vim" as the application name.


Any program specified with `Callback`/`callback` will be run after the screenshot has been taken. The filename will be supplied as the first argument and the same environment variables as for delegates (see below) will be set.

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
SCREENSHOTTER_NAME | The name screenshotter determined for the application, including any format strings or delegate output from matching overrides for callbacks but not yearly/monthly subdirs. Based on the process unless the process is dead, in which case it is the same as SCREENSHOTTER_APP_ID on Sway or WM_CLASS on X11.
SCREENSHOTTER_DIR | The absolute directory determined for the applications. Equivalent to screenshot_dir from the config + `/$SCREENSHOTTER_NAME`.
SCREENSHOTTER_WM_NAME | The name or title of the window from the window manager. May not be present.
SCREENSHOTTER_WINDOWID | X11 ony. The Window ID of the Window used to determine the active process.
SCREENSHOTTER_WINDOW_ID | Wayland/Sway only. The Window ID of the Window used to determine the active process. May not be stable ID on Sway.
SCREENSHOTTER_PID | The PID of the process that matched this Override. May not be present.
SCREENSHOTTER_MOUSEX | X11 only. The X coordinate of the cursor when the screenshot was taken.
SCREENSHOTTER_MOUSEY | X11 only. The Y coordinate of the cursor when the screenshot was taken.
SCREENSHOTTER_GEOMETRY | The geometry string of the selected window or region.
SCREENSHOTTER_APP_ID | Wayland/Sway only. The APP_ID from the window, or, for xwayland, the WM_CLASS instead. May not be present.
SCREENSHOTTER_WINDOW_PID | Wayland/Sway only. The original PID of the process from the window manager before ignoring processes.


The mouse coordinates may not be meaningful in `window` or `desktop` modes.
On X11 the format of the geometry will be: [{WIDTH}][x{HEIGHT}][{+-}{XOFF}[{+-}{YOFF}]] (e.g. "1436x879+6720+2160").
On Wayland/Sway the format will be: XOFF,YOFF WIDTHxHEIGHT (e.g. 6720,2160 1436x879)

<!-- TODO Add initial mouse coordinates on mousedown for region mode -->

# Limitations

The selection logic for determining which application is running in a window looks for the most recently spawned child process, which is not guaranteed to be the visible application in a terminal or shell. Delegates can be used solve this problem in some cases.

If selecting a region spanning multiple visible windows the application will be based on the window under the mouse cursor (x11) or center of the region (sway) when the user ends their selection.

