package main

import (
	"bytes"
	"fmt"
	"io/ioutil"
	"log"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"time"

	"github.com/BurntSushi/xgb/xproto"
	"github.com/BurntSushi/xgbutil"
	"github.com/BurntSushi/xgbutil/ewmh"
	"github.com/BurntSushi/xgbutil/xprop"
	"github.com/awused/awconf"
	"github.com/gen2brain/beeep"
	"github.com/shirou/gopsutil/process"
)

type override struct {
	Name   string
	Regex  string
	Format string
}

type config struct {
	RootDir            string
	Fallback           string
	YearlyApplications []string
	Overrides          []override
	IgnoredParents     []string
	CheckWindowID      bool
	Compression        int
	Callback           string
}

const (
	maybe int = iota
	no    int = iota
	yes   int = iota
)

// If _any_ have the right window ID then we must only match against those
var hasWindowID = maybe

var xu *xgbutil.XUtil
var c *config

var debug = false

// TODO -- urface/cli
func main() {
	if len(os.Args) < 2 {
		log.Fatal("Specify mode in [window, region, desktop]")
	}

	err := awconf.LoadConfig("screenshotter", &c)
	if err != nil {
		log.Fatal(err)
	}

	if !c.CheckWindowID {
		hasWindowID = no
	}

	// TODO -- better cli arg parsing
	if len(os.Args) > 2 && os.Args[2] == "--debug" {
		debug = true
	}

	tmpFile := mkTemp()
	appname := ""
	convertArgs := []string{
		tmpFile,
		"-define", "png:compression-level=" + strconv.Itoa(c.Compression),
	}
	defer os.Remove(tmpFile)

	// Take Screenshot and get application name
	if os.Args[1] == "window" {
		initXConn()
		appname = getActiveWindowApplication()
		screenshotActiveWindow(tmpFile)
	} else if os.Args[1] == "region" {
		initXConn()
		// TODO -- get window name at mousedown and compare to mouseup
		geometry := selectRegion()
		if geometry == "" {
			// Slop was cancelled
			return
		}
		appname = getMouseWindowApplication()
		// Faster to just screenshot the whole desktop and crop later
		convertArgs = append(convertArgs, "-crop", geometry)
		err := exec.Command(
			"gnome-screenshot",
			"-f", tmpFile).Run()
		if err != nil {
			panic("screenshot failed " + err.Error())
		}
	} else if os.Args[1] == "desktop" {
		appname = c.Fallback
		err := exec.Command(
			"gnome-screenshot",
			"-f", tmpFile).Run()
		if err != nil {
			panic("screenshot failed " + err.Error())
		}
	} else {
		panic("Specify mode in [window, region, desktop]")
	}

	if appname == "" {
		panic("Application name can't be empty, check your settings and overrides")
	}

	// Sanity check that a screenshot was taken
	fi, err := os.Stat(tmpFile)
	if err != nil {
		panic(err)
	}
	if fi.Size() == 0 {
		fmt.Println("No Screenshot was taken")
		return
	}

	outFile := getFileName(appname)

	err = os.MkdirAll(filepath.Dir(outFile), 0777)
	if err != nil {
		panic(err)
	}

	convertArgs = append(convertArgs, outFile)

	err = exec.Command("convert", convertArgs...).Run()
	if err != nil {
		panic(err)
	}

	if c.Callback != "" {
		err = exec.Command(c.Callback, outFile).Run()
		if err != nil {
			fmt.Println("Callback %s returned failed: %s", c.Callback, err)
		}
	}

	partPath := strings.TrimPrefix(outFile, c.RootDir)

	err = beeep.Notify("Screenshot Taken", partPath, outFile)
	if err != nil {
		panic(err)
	}
}

func getFileName(name string) string {
	path := filepath.Join(c.RootDir, name)
	d := time.Now()

	if contains(c.YearlyApplications, name) {
		return filepath.Join(
			path,
			d.Format("2006"),
			d.Format("01-02_15-04-05")+".png")
	}

	return filepath.Join(
		path,
		d.Format("2006-01-02_15-04-05")+".png")
}

func initXConn() {
	if xu == nil {
		X, err := xgbutil.NewConn()
		if err != nil {
			panic(err)
		}
		xu = X
	}
}

func mkTemp() string {
	f, err := ioutil.TempFile("", "screenshotter*.bmp")
	if err != nil {
		panic(err)
	}
	err = f.Close()
	if err != nil {
		panic(err)
	}

	n := f.Name()

	// Remove in Go 1.11
	if filepath.Ext(n) != ".bmp" {
		err = os.Rename(n, n+".bmp")
		if err != nil {
			panic(err)
		}
		n = n + ".bmp"
	}

	return n
}

func getActiveWindowApplication() string {
	wid, err := ewmh.ActiveWindowGet(xu)
	if err != nil {
		panic(err)
	}

	return getTargetProcess(wid)
}

func getMouseWindowApplication() string {
	out, err := exec.Command(
		"xdotool", "getmouselocation", "--shell").Output()
	if err != nil {
		panic(err)
	}

	re := regexp.MustCompile("WINDOW=([0-9]+)\n")

	matches := re.FindSubmatch(out)
	if matches == nil {
		return "desktop"
	}

	wid, err := strconv.Atoi(string(matches[1]))
	if err != nil {
		panic(err)
	}
	return getTargetProcess(xproto.Window(wid))
}

// Slop can give us window IDs but Slop will always give the root window for
// area selections, which is undesirable
func selectRegion() string {
	geometry, err := exec.Command("slop",
		"-n",
		"-f", "%g",
		"-l",
		"-c", "0,0,1,0.1").Output()
	if err != nil {
		return ""
	}

	return string(geometry)
}

func screenshotActiveWindow(file string) {
	// scrot can't do a single window, import -window misses compositor effects
	// gnome-screenshot should even work on wayland
	err := exec.Command("gnome-screenshot",
		"-w",
		"-B",
		"-f", file).Run()
	if err != nil {
		panic("screenshot failed " + err.Error())
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

// Naive linear search, fine for small numbers of items
// User would have to add tens of thousands of items to their configs for this
// to be noticeable
func contains(ss []string, s string) bool {
	for _, st := range ss {
		if st == s {
			return true
		}
	}
	return false
}

func procInWindow(p *process.Process, wid xproto.Window) bool {
	path := "/proc/" + strconv.Itoa(int(p.Pid)) + "/environ"

	env, err := ioutil.ReadFile(path)
	if err != nil {
		return false
	}

	return bytes.Contains(env, []byte("WINDOWID="+strconv.Itoa(int(wid))))
}

func youngestChild(p *process.Process, wid xproto.Window) *process.Process {
	children, err := p.Children()
	if err != nil && err != process.ErrorNoChildren {
		panic(err)
	}

	if len(children) == 0 {
		return nil
	}

	var child *process.Process
	var childtime int64

	for _, c := range children {
		if hasWindowID != no {
			if procInWindow(c, wid) {
				if hasWindowID != yes {
					hasWindowID = yes
					child = nil
				}
			} else if hasWindowID == yes {
				continue
			}
		}

		ctime, err := c.CreateTime()
		if err != nil {
			panic(err)
		}

		if child == nil || ctime > childtime {
			child, childtime = c, ctime
		}
	}

	if hasWindowID == maybe {
		// We didn't encounter any children with the WINDOWID so stop checking.
		hasWindowID = no
	}
	return child
}

// p can be nil
func overrideName(name string, p *process.Process) string {
	for _, o := range c.Overrides {
		// TODO -- check regex match
		if o.Name != name {
			continue
		}

		format := o.Format
		if p != nil && o.Regex != "" {
			re := regexp.MustCompile(o.Regex)

			cmdline, err := p.Cmdline()
			if err != nil {
				panic(err)
			}
			matches := re.FindStringSubmatch(cmdline)
			if matches == nil {
				continue
			}
			var interfaces []interface{} = make([]interface{}, len(matches))
			for i, m := range matches {
				interfaces[i] = m
			}
			format = fmt.Sprintf(format, interfaces...)

			if debug {
				fmt.Println(matches)
				fmt.Println(format)
			}
		}

		fName := convertApplicationName(format)

		if fName != "" {
			return fName
		}
	}
	return name
}

func getTargetProcess(wid xproto.Window) string {
	var name string
	var prc *process.Process

	pid, err := ewmh.WmPidGet(xu, wid)
	if err != nil {
		// No PID -> get window name
		name, err = ewmh.WmNameGet(xu, wid)
		if name == "" {
			// No _NET_WM_NAME -> get WM_NAME
			name, err = xprop.PropValStr(xprop.GetProperty(xu, wid, "WM_NAME"))
		}

		if err != nil {
			// No window name -> probably root window
			return convertApplicationName(c.Fallback)
		}

		name = convertApplicationName(name)
	} else {
		prc, err = process.NewProcess(int32(pid))
		if err != nil {
			panic(err)
		}

		if debug {
			fmt.Println(prc.Name())
			fmt.Println(prc.Exe())
			fmt.Println(prc.Cmdline())
		}

		// Name() can include flags and arguments
		pName, err := prc.Exe()
		if err != nil {
			panic(err)
		}

		name = convertApplicationName(filepath.Base(pName))

		for contains(c.IgnoredParents, name) {
			child := youngestChild(prc, wid)
			if child == nil {
				break
			}
			prc = child

			pName, err = prc.Name()
			if err != nil {
				panic(err)
			}

			name = convertApplicationName(filepath.Base(pName))
		}
	}

	return overrideName(name, prc)
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
