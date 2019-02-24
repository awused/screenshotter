#!/bin/sh
# Example delegate script that appends the mode to the name as a subdirectory
# unless the mode is desktop or "name".

# Don't match "desktop". The remaining overrides will be tried.
[ "$SCREENSHOTTER_MODE" = "desktop" ] && exit 1

# Do match "name", the application will be named "$SCREENSHOTTER_NAME"
[ "$SCREENSHOTTER_MODE" = "name" ] && exit 0

# Produces a nested directory like "firefox/window"
echo $SCREENSHOTTER_NAME
echo $SCREENSHOTTER_MODE
