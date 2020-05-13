Screenshotter
=============

A tool to take screenshots and organize them based on the name of the application in the screenshot. Supports full desktop screenshots, taking screenshots of windows, or selecting regions.

# Requirements

* Go 1.11 or newer
* ImageMagick 6
* [slop](https://github.com/naelstrof/slop)
* [maim](https://github.com/naelstrof/maim)
* An X11 based desktop environment
    * xdotool
* Any terminal emulator except gnome-terminal
    * Screenshots will work but it's impossible to correctly determine which application is running in a window.
    * Other libvte based terminals are fine.

# Usage

`go get -u github.com/awused/screenshotter`

Fill in screenshotter.toml and copy it to $HOME/.screenshotter.toml or $HOME/.config/screenshotter/screenshotter.toml.

Take a screenshot with `screenshotter {mode}` where mode is desktop, window (for the active window), region (to select a window or rectangular region), or name.

The `name` mode does not take a screenshot and can be used to ensure that your configuration is working properly without polluting your screenshots directory.

# Configuration

Most of the configuration in screenshotter.toml is straightforward but Overrides and IgnoredParents can get complicated.

Use IgnoredParents to cover the cases where you've started vim from zsh running in an xfce4-terminal window. Without any configuration the application name will be "xfce4-terminal", which is probably not what you want. By ignoring xfce4-terminal and zsh the tool will correctly detect "vim" as the application name.

When running terminals that share processes between windows (xfce4-terminal, gnome-terminal, etc) CheckWindowID must be set to true.

Any program specified with `Callback` will be run after the screenshot has been taken. The filename will be supplied as the first argument and the same environment variables as for delegates (see below) will be set.

## Overrides

Use Overrides when you want to manipulate how the directories are named for an application. In the simple cases you might want "chromium-browser" to instead be "chromium" or "vimx" to be "vim." In the more complicated cases you can use regular expressions and matching groups to change "python my-application.py" into  "my-application." Use the `-debug` flag or the `name` command to assist with editing these.

Use `Yearly` and `Monthly` to configure separate subfolders.

### Delegates

Specify a program using `Delegate` inside an Override block to delegate directory naming to another program or script for a particular program.

A delegate that exits with a non-zero status will be ignored and the next matching override will be attempted.

The delegate program will be executed with several environment variables set:

Environment Variable | Explanation
-------------------- | ----------
SCREENSHOTTER_MODE | The mode used when calling screenshotter.
SCREENSHOTTER_NAME | The name screenshotter determined for the application, including any format strings from overrides.
SCREENSHOTTER_DIR | The directory name determined for the applications. For delegates this is the same as the name, for callbacks this reflects the output of any delegates but not whether it uses yearly/monthly directories.
SCREENSHOTTER_WINDOWID | The Window ID of the Window used to determine the active process.
SCREENSHOTTER_PID | The PID of the process that matched this Override. May not be present.
SCREENSHOTTER_MOUSEX | The X coordinate of the cursor when the screenshot was taken.
SCREENSHOTTER_MOUSEY | The Y coordinate of the cursor when the screenshot was taken.
SCREENSHOTTER_GEOMETRY | The geometry string of the selected window or region.

The mouse coordinates may not be meaningful in `window` or `desktop` modes.
The format of the geometry will use the X11 geometry format: [{WIDTH}][x{HEIGHT}][{+-}{XOFF}[{+-}{YOFF}]] (e.g. "1436x879+6720+2160").

<!-- TODO Add initial mouse coordinates on mousedown for region mode -->

# Limitations

The selection logic for determining which application is running in a window looks for the most recently spawned child process, which is not guaranteed to be the visible application in a terminal or shell. Delegates can be used solve this problem in some cases.

If selecting a region spanning multiple visible windows the application will be based on the window under the mouse cursor when the user ends their selection. In the future this might be made smarter, to check if more than one application is visible in the screenshot and use the configurable fallback name, but this naive approach happens to match the behaviour of sharex.

