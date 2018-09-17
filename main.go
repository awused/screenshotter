package main

import (
	"fmt"
	"io/ioutil"
	"log"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"

	"github.com/BurntSushi/xgb/xproto"
	"github.com/BurntSushi/xgbutil"
	"github.com/BurntSushi/xgbutil/ewmh"
	"github.com/awused/awconf"
	"github.com/shirou/gopsutil/process"
)

type config struct {
	RootDir            string
	Fallback           string
	YearlyApplications []string
	// TODO -- application overrides from name -> name
}

// TODO -- urface/cli
func main() {
	if len(os.Args) < 2 {
		log.Fatal("Specify mode in [window, section]")
	}

	var c config

	err := awconf.LoadConfig("screenshotter", &c)
	if err != nil {
		log.Fatal(err)
	}

	outfile := mkTemp()
	appname := ""
	defer os.Remove(outfile)

	// Take Screenshot and get application name
	if os.Args[1] == "window" {
		appname = getActiveWindowCommand()
		screenshotActiveWindow(outfile)

		fmt.Println(appname)
	} else if os.Args[1] == "section" {
		// TODO -- get window name at mousedown and compare to mouseup
	} else {
		log.Fatal("Specify mode in [window, section]")
	}

	// Get date

	// Move and convert screenshot (we take a bmp screenshot for speed)

	// Send notification and set clipboard
}

func mkTemp() string {
	f, err := ioutil.TempFile("", "screenshotter*.bmp")
	if err != nil {
		log.Fatal(err)
	}
	err = f.Close()
	if err != nil {
		log.Fatal(err)
	}

	n := f.Name()

	// Remove in Go 1.11
	if filepath.Ext(n) != ".bmp" {
		err = os.Rename(n, n+".bmp")
		if err != nil {
			log.Fatal(err)
		}
		n = n + ".bmp"
	}

	return n
}

func getActiveWindowCommand() string {
	X, err := xgbutil.NewConn()
	if err != nil {
		log.Fatal(err)
	}

	wid, err := ewmh.ActiveWindowGet(X)
	if err != nil {
		log.Fatal(err)
	}

	return getWindowCommand(X, wid)
}

func screenshotActiveWindow(file string) {
	// scrot can't do a single window, import -window misses compositor effects
	// gnome-screenshot should even work on wayland
	err := exec.Command("gnome-screenshot", "-w", "-f", file).Run()
	if err != nil {
		log.Fatal("screenshot failed " + err.Error())
	}
}

var safeFilenameRegex = regexp.MustCompile(`[^\p{L}\p{N}-_+=]+`)
var repeatedHyphens = regexp.MustCompile(`--+`)

func convertApplicationName(input string) string {
	output := strings.ToLower(input)
	output = safeFilenameRegex.ReplaceAllString(output, "-")
	output = repeatedHyphens.ReplaceAllString(output, "-")
	return strings.Trim(output, "-")
}

func getWindowCommand(X *xgbutil.XUtil, wid xproto.Window) string {
	pid, err := ewmh.WmPidGet(X, wid)
	if err != nil {
		log.Fatal(err)
	}

	prc, err := process.NewProcess(int32(pid))
	if err != nil {
		log.Fatal(err)
	}

	name, err := prc.Name()
	if err != nil {
		log.Fatal(err)
	}

	return convertApplicationName(filepath.Base(name))
}

func runBash(cmd string) (string, error) {
	// See http://redsymbol.net/articles/unofficial-bash-strict-mode/
	command := `
		set -euo pipefail
		IFS=$'\n\t'
		` + cmd + "\n"

	bash := exec.Command("/usr/bin/env", "bash")
	bash.Stdin = strings.NewReader(command)
	bash.Stderr = os.Stderr

	bashOut, err := bash.Output()
	return string(bashOut), err
}
