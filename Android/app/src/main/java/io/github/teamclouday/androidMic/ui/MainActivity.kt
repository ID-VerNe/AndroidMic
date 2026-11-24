package io.github.teamclouday.androidMic.ui

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Bundle
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.viewModels
import androidx.core.content.ContextCompat
import io.github.teamclouday.androidMic.AndroidMicApp
import io.github.teamclouday.androidMic.domain.service.WAIT_PERIOD
import io.github.teamclouday.androidMic.ui.home.HomeScreen
import io.github.teamclouday.androidMic.ui.home.openAppSettings
import io.github.teamclouday.androidMic.ui.theme.AndroidMicTheme
import io.github.teamclouday.androidMic.ui.utils.rememberWindowInfo
import io.github.teamclouday.androidMic.utils.ignore

private const val TAG = "MainActivity"

/** Hidden framework broadcast/extra used by UsbManager; not in the public SDK. */
private const val ACTION_USB_STATE = "android.hardware.usb.action.USB_STATE"
private const val EXTRA_USB_CONNECTED = "connected"

class MainActivity : ComponentActivity() {

    companion object {
        /**
         * Extra used by the PC (`adb shell am start ... --ez <extra> true`) to
         * wake the app and ask it to start the ADB stream.
         */
        const val EXTRA_AUTO_CONNECT = "io.github.teamclouday.androidMic.extra.AUTO_CONNECT"

        /**
         * LAN address and port the PC pushes with the launch intent, so an
         * `AUTO` phone can fall back to Wi-Fi without a manual IP entry.
         */
        const val EXTRA_PC_IP = "io.github.teamclouday.androidMic.extra.PC_IP"
        const val EXTRA_PC_PORT = "io.github.teamclouday.androidMic.extra.PC_PORT"
    }

    val vm: MainViewModel by viewModels()

    private var usbReceiverRegistered = false

    private val usbStateReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            if (intent?.action != ACTION_USB_STATE) return
            if (!intent.getBooleanExtra(EXTRA_USB_CONNECTED, false)) return

            Log.d(TAG, "USB attached")
            vm.requestAutoConnect()
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Log.d(TAG, "onCreate")

        if (intent?.getBooleanExtra(EXTRA_AUTO_CONNECT, false) == true) {
            Log.d(TAG, "onCreate -> auto connect extra")
            vm.requestAutoConnect(
                pcIp = intent.getStringExtra(EXTRA_PC_IP),
                pcPort = intent.getIntExtra(EXTRA_PC_PORT, -1)
            )
        }

        setContent {

            AndroidMicTheme(
                theme = vm.prefs.theme.getAsState().value,
                dynamicColor = vm.prefs.dynamicColor.getAsState().value
            ) {
                // get windowInfo for rotation change
                val windowInfo = rememberWindowInfo()

                HomeScreen(vm, windowInfo, openAppSettings = ::openAppSettings)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        if (intent.getBooleanExtra(EXTRA_AUTO_CONNECT, false)) {
            Log.d(TAG, "onNewIntent -> auto connect extra")
            vm.requestAutoConnect(
                pcIp = intent.getStringExtra(EXTRA_PC_IP),
                pcPort = intent.getIntExtra(EXTRA_PC_PORT, -1)
            )
        }
        if (intent.extras?.getBoolean("ForegroundServiceBound") == true) {
            Log.d(TAG, "onNewIntent -> ForegroundServiceBound")
            // get variable from application
            vm.refreshAppVariables()
            // get status
            vm.askForStatus()
        }
    }

    override fun onResume() {
        super.onResume()
        if (!usbReceiverRegistered) {
            ContextCompat.registerReceiver(
                this,
                usbStateReceiver,
                IntentFilter(ACTION_USB_STATE),
                ContextCompat.RECEIVER_EXPORTED
            )
            usbReceiverRegistered = true
        }
        // onStart's bind can be rejected while the activity is not yet
        // foreground (cold launch from adb with the screen off); retry once
        // the activity is actually visible.
        if (!AndroidMicApp.isBound) {
            (application as AndroidMicApp).bindService()
        }
    }

    override fun onPause() {
        super.onPause()
        if (usbReceiverRegistered) {
            unregisterReceiver(usbStateReceiver)
            usbReceiverRegistered = false
        }
    }

    override fun onStart() {
        super.onStart()
        Log.d(TAG, "onStart")


        vm.handlerServiceResponse()
        // get variable from application
        vm.refreshAppVariables()

        (application as AndroidMicApp).bindService()
    }


    override fun onStop() {
        super.onStop()
        Log.d(TAG, "onStop")
        vm.messengerLooper.quitSafely()
        ignore { vm.handlerThread.join(WAIT_PERIOD) }

        (application as AndroidMicApp).unBindService()
    }

}