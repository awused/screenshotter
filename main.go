package main

import (
	"flag"
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
	"github.com/awused/awconf"
	"github.com/gen2brain/beeep"
)

type override struct {
	Name     string
	Regex    string
	Format   string
	Yearly   bool
	Monthly  bool
	Delegate string
	Callback string
}

type config struct {
	ScreenshotDir      string
	Fallback           string
	YearlyApplications *[]string
	Overrides          []override
	IgnoredParents     []string
	CheckWindowID      bool
	Compression        int
	SlopShaders        []string
	Callback           string
	MouseKeys          bool
}

type application struct {
	dir      string
	yearly   bool
	monthly  bool
	callback string
}

const (
	no int = iota
	yes
	maybe
)

var stderr = log.New(os.Stderr, "", 0)

// If _any_ have the right window ID then we must only match against those
var hasWindowID = maybe

var xu *xgbutil.XUtil
var c *config

var appChan = make(chan application)
var errorChan = make(chan error)

var delegateEnvironment = make(map[string]string)

var mode = ""
var debug = false

// TODO -- urfave/cli instead of this mess
func main() {
	flag.BoolVar(&debug, "debug", false, "Enable debugging output")
	flag.Parse()
	if flag.NArg() == 0 {
		log.Fatal("Specify mode in [window, region, desktop]")
	}

	err := awconf.LoadConfig("screenshotter", &c)
	if err != nil {
		log.Fatal(err)
	}

	if c.YearlyApplications != nil {
		_ = beeep.Notify(
			"Screenshotter Error",
			"YearlyApplications has been removed in favour of more flexible "+
				"Override settings. Update your config.", "")
		os.Exit(1)
	}

	if !c.CheckWindowID {
		hasWindowID = no
	}

	// It'd be slightly faster to connect maim and imagemagick using pipes but
	// not worth the complexity.
	tmpBMP, tmpPNG := mkTemp()
	screenshotArgs := []string{
		"--capturebackground",
		"--hidecursor",
		"--quality", "1", // Slow, but as good as we can manage
	}
	defer os.Remove(tmpBMP)
	defer os.Remove(tmpPNG)

	initXConn()

	mode = flag.Arg(0)
	delegateEnvironment["SCREENSHOTTER_MODE"] = mode

	switch mode {
	case "window":
		wid, err := ewmh.ActiveWindowGet(xu)
		if err != nil {
			errorChan <- err
			return
		}

		go getActiveWindowApplication(wid)
		screenshotArgs = append(
			screenshotArgs,
			"--window", strconv.Itoa(int(wid)),
		)
	case "desktop":
		go getDesktopApplicationName()
	case "region":
		geometry := selectRegion()
		if geometry == "" {
			// Slop was cancelled
			return
		}
		delegateEnvironment["SCREENSHOTTER_GEOMETRY"] = geometry
		go getMouseWindowApplication()
		screenshotArgs = append(
			screenshotArgs,
			"--geometry", geometry,
		)
	case "name":
		debug = true // name implies debug
		// TODO -- get window name at mousedown and compare to mouseup
		geometry := selectRegion()
		if geometry == "" {
			// Slop was cancelled
			return
		}
		delegateEnvironment["SCREENSHOTTER_GEOMETRY"] = geometry
		go getMouseWindowApplication()

		select {
		case app := <-appChan:
			fname := getFileName(app)
			stderr.Println("Application directory: " + app.dir)
			stderr.Println("File name: " + fname)
			if c.Callback != "" {
				stderr.Printf("Would call callback [%s] with environment:\n", c.Callback)

				for k, v := range delegateEnvironment {
					stderr.Println(k + "=" + v)
				}
			}

			err = beeep.Notify("Application Directory", app.dir, "")
			if err != nil {
				panic(err)
			}
			fmt.Println(fname)
		case err = <-errorChan:
			panic(err)
		}
		return
	case "window-name":
		debug = true // name implies debug
		wid, err := strconv.ParseUint(flag.Arg(1), 10, 32)
		if err != nil {
			panic(err)
		}
		go getActiveWindowApplication(xproto.Window(uint32(wid)))

		select {
		case app := <-appChan:
			fname := getFileName(app)
			stderr.Println("Application directory: " + app.dir)
			stderr.Println("File name: " + fname)
			if c.Callback != "" {
				stderr.Printf("Would call callback [%s] with environment:\n", c.Callback)

				for k, v := range delegateEnvironment {
					stderr.Println(k + "=" + v)
				}
			}

			err = beeep.Notify("Application Directory", app.dir, "")
			if err != nil {
				panic(err)
			}
			fmt.Println(fname)
		case err = <-errorChan:
			panic(err)
		}
		return
	default:
		stderr.Println("Specify mode in [window, region, desktop, name]")
		return
	}

	screenshotArgs = append(screenshotArgs, tmpBMP)

	err = exec.Command("maim", screenshotArgs...).Run()
	if err != nil {
		panic(err)
	}

	convertArgs := []string{
		tmpBMP,
		"-background", "black",
		"-alpha", "off",
		"-define", "png:compression-level=" + strconv.Itoa(c.Compression),
		tmpPNG,
	}

	err = exec.Command("convert", convertArgs...).Run()
	if err != nil {
		panic(err)
	}

	var app application

	select {
	case app = <-appChan:
	case err = <-errorChan:
		panic(err)
	}

	if app.dir == "" {
		panic("Application name can't be empty, check your settings and overrides")
	}

	outFile := getFileName(app)

	if debug {
		stderr.Println("Application directory: " + app.dir)
		stderr.Println("File name: " + outFile)
	}

	err = os.MkdirAll(filepath.Dir(outFile), 0777)
	if err != nil {
		panic(err)
	}

	err = moveFile(tmpPNG, outFile)
	if err != nil {
		panic(err)
	}

	if app.callback != "" {
		if debug {
			stderr.Printf("Calling override callback [%s] with environment:\n",
				app.callback)
		}
		cmd := exec.Command(app.callback, outFile)
		cmd.Env = os.Environ()
		for k, v := range delegateEnvironment {
			if debug {
				stderr.Println(k + "=" + v)
			}
			cmd.Env = append(cmd.Env, k+"="+v)
		}

		err = cmd.Run()
		if err != nil {
			stderr.Printf("Override callback [%s] failed: %s\n", app.callback, err)
		}
	}

	if c.Callback != "" {
		if debug {
			stderr.Printf("Calling callback [%s] with environment:\n", c.Callback)
		}
		cmd := exec.Command(c.Callback, outFile)
		cmd.Env = os.Environ()
		for k, v := range delegateEnvironment {
			if debug {
				stderr.Println(k + "=" + v)
			}
			cmd.Env = append(cmd.Env, k+"="+v)
		}

		err = cmd.Run()
		if err != nil {
			stderr.Printf("Callback [%s] failed: %s\n", c.Callback, err)
		}
	}

	partPath := strings.TrimPrefix(outFile, c.ScreenshotDir)

	err = beeep.Notify("Screenshot Taken", partPath, outFile)
	if err != nil {
		panic(err)
	}
}

func getFileName(app application) string {
	path := filepath.Join(c.ScreenshotDir, app.dir)
	d := time.Now()

	if app.monthly {
		if app.yearly {
			return filepath.Join(
				path,
				d.Format("2006"),
				d.Format("01"),
				d.Format("02_15-04-05")+".png")
		}
		return filepath.Join(
			path,
			d.Format("2006-01"),
			d.Format("02_15-04-05")+".png")
	} else if app.yearly {
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

func mkTemp() (string, string) {
	f, err := ioutil.TempFile("", "screenshotter*.bmp")
	if err != nil {
		panic(err)
	}
	err = f.Close()
	if err != nil {
		panic(err)
	}

	tmpBMP := f.Name()

	// Remove in Go 1.11
	if filepath.Ext(tmpBMP) != ".bmp" {
		panic("Screenshotter requires Go 1.11 or higher")
	}

	f, err = ioutil.TempFile("", "screenshotter*.png")
	if err != nil {
		panic(err)
	}
	err = f.Close()
	if err != nil {
		panic(err)
	}

	tmpPNG := f.Name()

	return tmpBMP, tmpPNG
}

// Slop can give us window IDs but Slop will always give the root window for
// area selections, which is undesirable
func selectRegion() string {
	if c.MouseKeys {
		xsetOut, err := exec.Command("xset", "q").Output()
		if err != nil {
			return ""
		}

		disabled, err := regexp.Match("Mouse Keys: +off", xsetOut)

		if disabled {
			err = exec.Command("xset", "led", "named", "Mouse Keys").Run()
			if err != nil {
				return ""
			}
			defer exec.Command("xset", "-led", "named", "Mouse Keys").Run()
		}
	}

	slopArgs := []string{
		"-n",
		"-f", "%g",
		"-l",
		"-c", "0,1,1,0.3",
	}

	if len(c.SlopShaders) > 0 {
		slopArgs = append(
			slopArgs,
			"-r", strings.Join(c.SlopShaders, ","))
	}

	geometry, err := exec.Command("slop", slopArgs...).Output()
	if err != nil {
		return ""
	}

	return string(geometry)
}

func moveFile(inFile string, outFile string) error {
	err := os.Rename(inFile, outFile)
	if err != nil {
		// Probably a cross-device link or other error from os.Rename
		err = exec.Command("mv", inFile, outFile).Run()
	}

	if err != nil {
		return err
	}

	// Set some sane permissions
	return os.Chmod(outFile, 0664)
}
