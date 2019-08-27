package main

import (
	"flag"
	"fmt"
	"io/ioutil"
	"log"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/BurntSushi/xgbutil"
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
}

type config struct {
	ScreenshotDir      string
	Fallback           string
	YearlyApplications *[]string
	Overrides          []override
	IgnoredParents     []string
	CheckWindowID      bool
	Compression        int
	Callback           string
}

type application struct {
	dir     string
	yearly  bool
	monthly bool
}

const (
	no int = iota
	yes
	maybe
)

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

	tmpBMP, tmpPNG := mkTemp()
	convertArgs := []string{
		tmpBMP,
		"-define", "png:compression-level=" + strconv.Itoa(c.Compression),
	}
	defer os.Remove(tmpBMP)
	defer os.Remove(tmpPNG)

	initXConn()

	mode = flag.Arg(0)
	delegateEnvironment["SCREENSHOTTER_MODE"] = mode

	switch mode {
	case "window":
		go getActiveWindowApplication()
		screenshotActiveWindow(tmpBMP)
	case "desktop":
		go getDesktopApplicationName()
		err := exec.Command(
			"gnome-screenshot",
			"-f", tmpBMP).Run()
		if err != nil {
			panic("screenshot failed " + err.Error())
		}
	case "region":
		geometry := selectRegion()
		if geometry == "" {
			// Slop was cancelled
			return
		}
		delegateEnvironment["SCREENSHOTTER_GEOMETRY"] = geometry
		go getMouseWindowApplication()
		// Faster to just screenshot the whole desktop and crop later
		convertArgs = append(convertArgs, "-crop", geometry)
		err := exec.Command(
			"gnome-screenshot",
			"-f", tmpBMP).Run()
		if err != nil {
			panic("screenshot failed " + err.Error())
		}
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
			fmt.Println("Application directory: " + app.dir)
			fmt.Println("File name: " + getFileName(app))
			if c.Callback != "" {
				fmt.Printf("Would call callback [%s] with environment:\n", c.Callback)

				for k, v := range delegateEnvironment {
					fmt.Println(k + "=" + v)
				}
			}

			err = beeep.Notify("Application Directory", app.dir, "")
			if err != nil {
				panic(err)
			}
		case err = <-errorChan:
			panic(err)
		}
		return
	default:
		panic("Specify mode in [window, region, desktop, name]")
	}

	// Sanity check that a screenshot was taken
	fi, err := os.Stat(tmpBMP)
	if err != nil {
		panic(err)
	}
	if fi.Size() == 0 {
		fmt.Println("No Screenshot was taken")
		return
	}

	convertArgs = append(convertArgs, tmpPNG)

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
		fmt.Println("Application directory: " + app.dir)
		fmt.Println("File name: " + outFile)
	}

	err = os.MkdirAll(filepath.Dir(outFile), 0777)
	if err != nil {
		panic(err)
	}

	err = moveFile(tmpPNG, outFile)
	if err != nil {
		panic(err)
	}

	if c.Callback != "" {
		if debug {
			fmt.Printf("Calling callback [%s] with environment:\n", c.Callback)
		}
		cmd := exec.Command(c.Callback, outFile)
		cmd.Env = os.Environ()
		for k, v := range delegateEnvironment {
			if debug {
				fmt.Println(k + "=" + v)
			}
			cmd.Env = append(cmd.Env, k+"="+v)
		}

		err = cmd.Run()
		if err != nil {
			fmt.Printf("Callback [%s] failed: %s\n", c.Callback, err)
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
		} else {
			return filepath.Join(
				path,
				d.Format("2006-01"),
				d.Format("02_15-04-05")+".png")
		}
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
