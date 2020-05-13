package main

import (
	"bytes"
	"errors"
	"fmt"
	"io/ioutil"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"

	"github.com/BurntSushi/xgb/xproto"
	"github.com/BurntSushi/xgbutil/ewmh"
	"github.com/BurntSushi/xgbutil/xprop"
	"github.com/BurntSushi/xgbutil/xwindow"
	"github.com/shirou/gopsutil/process"
)

var noWindowError = errors.New("No window under mouse")

func getActiveWindowApplication(wid xproto.Window) {
	err := setDelegateVariablesForFullWindow(wid)
	if err != nil {
		errorChan <- err
		return
	}
	err = getTargetApplication(wid)
	if err != nil {
		errorChan <- err
	}
}

func getDesktopApplicationName() {
	wid := xu.RootWin()

	err := setDelegateVariablesForFullWindow(wid)
	if err != nil {
		errorChan <- err
		return
	}
	err = overrideApplication(c.Fallback, nil)
	if err != nil {
		errorChan <- err
	}
}

func getMouseWindowApplication() {
	wid, err := getMouseInfo()
	if err == noWindowError {
		err = overrideApplication(c.Fallback, nil)
		if err != nil {
			errorChan <- err
		}
		return

	} else if err != nil {
		errorChan <- err
		return
	}

	delegateEnvironment["SCREENSHOTTER_WINDOWID"] = strconv.Itoa(int(wid))

	err = getTargetApplication(xproto.Window(wid))
	if err != nil {
		errorChan <- err
	}
}

func setDelegateVariablesForFullWindow(wid xproto.Window) error {
	geom, err := getWindowGeometry(wid)
	if err != nil {
		return err
	}

	delegateEnvironment["SCREENSHOTTER_WINDOWID"] = strconv.Itoa(int(wid))
	delegateEnvironment["SCREENSHOTTER_GEOMETRY"] = geom

	_, err = getMouseInfo()
	if err != nil && err != noWindowError {
		return err
	}
	return nil
}

// Sets the MOUSEX and MOUSEY delegate environment variables
// Returns the ID of the window under the cursor
func getMouseInfo() (int, error) {
	out, err := exec.Command(
		"xdotool", "getmouselocation", "--shell").Output()
	if err != nil {
		return 0, err
	}

	x, err := getVarFromXdotool(out, "X")
	if err != nil {
		return 0, err
	}

	y, err := getVarFromXdotool(out, "Y")
	if err != nil {
		return 0, err
	}

	delegateEnvironment["SCREENSHOTTER_MOUSEX"] = x
	delegateEnvironment["SCREENSHOTTER_MOUSEY"] = y

	wid, err := getVarFromXdotool(out, "WINDOW")
	if err != nil {
		return 0, noWindowError
	}

	return strconv.Atoi(wid)
	//return getTargetProcess(xproto.Window(wid))
}

// Get the geometry without decorations
// Should match slop when selecting a window
// xdotool getwindowgeometry is simply wrong by any measure, don't use it1
// xgbutil only returns relative coordinates
func getWindowGeometry(wid xproto.Window) (string, error) {
	win := xwindow.New(xu, wid)
	rect, err := win.Geometry()
	if err != nil {
		return "", err
	}

	rootID := xu.RootWin()
	x, y := rect.X(), rect.Y()

	for win.Id != rootID {
		pwin, err := win.Parent()
		if err != nil {
			// Note that win.Parent is badly programmed and panics instead of
			// returning an error.
			break
		}

		r, err := pwin.Geometry()
		if err != nil {
			return "", err
		}

		x += r.X()
		y += r.Y()

		win = pwin
	}

	return fmt.Sprintf("%dx%d%+d%+d", rect.Width(), rect.Height(), x, y), nil
}

func getTargetApplication(wid xproto.Window) error {
	var name string
	var prc *process.Process

	pid, err := ewmh.WmPidGet(xu, wid)
	if err != nil {
		// No PID -> get window name
		name, err := ewmh.WmNameGet(xu, wid)
		if name == "" {
			// No _NET_WM_NAME -> get WM_NAME
			name, err = xprop.PropValStr(xprop.GetProperty(xu, wid, "WM_NAME"))
		}

		if err != nil {
			// No window name -> probably root window
			name = convertApplicationName(c.Fallback)
		}

		name = convertApplicationName(name)
	} else {
		prc, err = process.NewProcess(int32(pid))
		if err != nil {
			return err
		}

		if debug {
			// fmt.Println("Name: " + fmt.Sprint(prc.Name()))
			fmt.Println("Executable: " + fmt.Sprint(prc.Exe()))
			fmt.Println("Command Line: " + fmt.Sprint(prc.Cmdline()))
		}

		// Name() can include flags and arguments
		pName, err := prc.Exe()
		if err != nil {
			return err
		}

		// This could be undesirable if the executable does end with " (deleted)"
		pName = strings.TrimSuffix(pName, " (deleted)")
		name = convertApplicationName(filepath.Base(pName))

		for contains(c.IgnoredParents, name) {
			child, err := youngestChild(prc, wid)
			if err != nil {
				return err
			}
			if child == nil {
				break
			}
			prc = child

			if debug {
				// fmt.Println("Name: " + fmt.Sprint(prc.Name()))
				fmt.Println("Executable: " + fmt.Sprint(prc.Exe()))
				fmt.Println("Command Line: " + fmt.Sprint(prc.Cmdline()))
			}

			pName, err = prc.Exe()
			if err != nil {
				return err
			}

			pName = strings.TrimSuffix(pName, " (deleted)")
			name = convertApplicationName(filepath.Base(pName))
		}

		delegateEnvironment["SCREENSHOTTER_PID"] = strconv.Itoa(int(prc.Pid))
	}

	return overrideApplication(name, prc)
}

func youngestChild(p *process.Process, wid xproto.Window) (
	*process.Process, error) {
	children, err := p.Children()
	if err != nil && err != process.ErrorNoChildren {
		return nil, err
	}

	if len(children) == 0 {
		return nil, nil
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
			return nil, err
		}

		if child == nil || ctime > childtime {
			child, childtime = c, ctime
		}
	}

	if hasWindowID == maybe {
		// We didn't encounter any children with the WINDOWID so stop checking.
		hasWindowID = no
	}
	return child, nil
}

func procInWindow(p *process.Process, wid xproto.Window) bool {
	path := "/proc/" + strconv.Itoa(int(p.Pid)) + "/environ"

	env, err := ioutil.ReadFile(path)
	if err != nil {
		return false
	}

	return bytes.Contains(env, []byte("WINDOWID="+strconv.Itoa(int(wid))))
}

// p can be nil
func overrideApplication(name string, p *process.Process) error {
	if debug {
		fmt.Println("Name before overrides: " + name)
	}

	app := application{dir: name}
	delegateEnvironment["SCREENSHOTTER_NAME"] = name
	delegateEnvironment["SCREENSHOTTER_DIR"] = name

	for _, o := range c.Overrides {
		delegateEnvironment["SCREENSHOTTER_NAME"] = name
		delegateEnvironment["SCREENSHOTTER_DIR"] = name

		var matches []string
		delegateName := ""
		fName := ""

		if o.Name != "" && o.Name != name {
			continue
		}

		if o.Regex != "" {
			if p == nil {
				continue
			}

			re := regexp.MustCompile(o.Regex)

			cmdline, err := p.Cmdline()
			if err != nil {
				return err
			}
			matches = re.FindStringSubmatch(cmdline)
			if matches == nil {
				continue
			}
		}

		if o.Format != "" {
			var interfaces []interface{} = make([]interface{}, len(matches))
			for i, m := range matches {
				interfaces[i] = m
			}
			fName = fmt.Sprintf(o.Format, interfaces...)

			fName = convertApplicationName(fName)

			if fName != "" {
				delegateEnvironment["SCREENSHOTTER_NAME"] = fName
				delegateEnvironment["SCREENSHOTTER_DIR"] = fName
			}
		}

		if o.Delegate != "" {
			output, matched := runDelegate(o.Delegate)
			if !matched {
				continue
			}
			delegateName = output
			delegateEnvironment["SCREENSHOTTER_DIR"] = output
		}

		if debug {
			fmt.Printf("Matching override: %+v\n", o)
		}

		app.yearly = o.Yearly
		app.monthly = o.Monthly
		app.callback = o.Callback

		if delegateName != "" {
			app.dir = delegateName
		} else if fName != "" {
			app.dir = fName
		}

		break
	}
	appChan <- app

	return nil
}

func runDelegate(delegate string) (string, bool) {
	if debug {
		fmt.Printf("Calling delegate [%s] with environment:\n", delegate)
	}
	cmd := exec.Command(delegate)
	cmd.Env = os.Environ()
	for k, v := range delegateEnvironment {
		if debug {
			fmt.Println(k + "=" + v)
		}
		cmd.Env = append(cmd.Env, k+"="+v)
	}

	stdout, err := cmd.Output()
	out := string(stdout)
	if debug && out != "" {
		fmt.Println("Raw delegate output:\n" + out)
	}

	if err != nil {
		if debug {
			fmt.Println("Delegate exited with error: " + err.Error())
		}

		return "", false
	}

	dirs := make([]string, 0)
	for _, s := range strings.Split(out, "\n") {
		s = convertApplicationName(s)
		if s != "" {
			dirs = append(dirs, s)
		}
	}

	dir := filepath.Join(dirs...)
	if debug {
		fmt.Println("Delegate directory: " + dir)
	}
	return dir, true
}
