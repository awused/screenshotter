Screenshotter
=============

A tool to take screenshots and organize them based on the name of the application in the screenshot. Supports full desktop screenshots, taking screenshots of windows, or selecting regions.

# Requirements

* ImageMagick 6
* [slop](https://github.com/naelstrof/slop)
* gnome-screenshot
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

Use Overrides when you want to manipulate how an application shows up. In the simple cases you might want "chromium-browser" to instead be "chromium" or "vimx" to be "vim." In the more complicated cases you can use regular expressions and matching groups to change "python my-application.py" into  "my-application." Use the `--debug` flag or the `name` command to assist with editing these.

When running terminals that share processes between windows (xfce4-terminal, gnome-terminal, etc) CheckWindowID must be set to true.

# Limitations

The selection logic for determining which application is running in a window looks for the most recently spawned child process, which is not guaranteed to be the visible application in a terminal or shell.

The application logic does not currently work at all with terminal multiplexers like tmux or screen and it can't tell which application is being run in the active pane. With work I believe tmux support can be added but it hasn't been a priority.

If selecting a region spanning multiple visible windows the application will be based on the window under the mouse cursor when the user ends their selection. In the future this might be made smarter, to check if more than one application is visible in the screenshot and use the configurable fallback name, but this naive approach happens to match the behaviour of sharex.
