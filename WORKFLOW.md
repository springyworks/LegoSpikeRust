# Hassle-Free Rust Development Workflow for LEGO Spike

## What We Built
- A Rust application that displays a 5x5 smiley face on the LED matrix
- The app automatically reboots the Hub into DFU mode when it finishes
- A deployment script that waits for DFU mode and flashes automatically

## One-Time Setup (Manual DFU)

### Step 1: Put Hub in DFU Mode Manually
1. **Disconnect** the Hub from USB
2. **Hold down** the Bluetooth button (center button)
3. **While holding**, plug in the USB cable
4. **Keep holding** until the button LED flashes pink/blue
5. **Release** the button - Hub is now in DFU mode

### Step 2: Flash the "Auto-Reboot" Version
```bash
./deploy_rust_led.sh
```
This builds and flashes our special Rust app that can reboot itself.

### Step 3: Watch It Run
- Hub reboots and shows "Rust Image!" 
- Displays smiley face for 5 seconds
- Blinks green light 3 times
- **Automatically reboots back to DFU mode**

---

## Future Development (No Manual Steps!)

### The Automated Loop
1. **Modify** your Rust code in `spike-rt/sample/rust_led/rust_app/src/lib.rs`
2. **Run** the script:
   ```bash
   ./deploy_rust_led.sh
   ```
3. **Wait** - The script will:
   - Build your code
   - Wait for the Hub to be in DFU mode
   - Flash automatically when ready
   - Hub runs your new code
   - Hub reboots to DFU mode when done

### What You See
```
$ ./deploy_rust_led.sh
Building Rust LED application...
Waiting for Hub to enter DFU mode...
DFU device found!
Flashing application...
Done! The Hub should restart automatically.
```

### If Hub is Not in DFU Mode
- If previous app is running, just wait - it will auto-reboot to DFU
- If Hub is off, turn it on and wait
- If stuck, press center button to reset, then wait

## No More Button Holding!
After the one-time setup, you never need to:
- Hold buttons
- Unplug USB cables  
- Manually enter DFU mode

The workflow becomes: **Edit → Run Script → Wait → Test → Repeat**