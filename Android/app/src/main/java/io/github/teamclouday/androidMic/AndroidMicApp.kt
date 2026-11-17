package io.github.teamclouday.androidMic

import android.app.Application
import android.content.ComponentName
import android.content.Intent
import android.content.ServiceConnection
import android.os.IBinder
import android.os.Messenger
import android.util.Log
import io.github.teamclouday.androidMic.domain.service.BIND_SERVICE_ACTION
import io.github.teamclouday.androidMic.domain.service.ForegroundService
import io.github.teamclouday.androidMic.ui.MainActivity
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.launch


private const val TAG = "AndroidMicApp"

class AndroidMicApp : Application() {


    companion object {
        lateinit var appModule: AppModule
        var service: Messenger? = null
        var isBound = false
        private var isBinding = false
    }

    private val scope = MainScope()
    override fun onCreate() {
        super.onCreate()

        appModule = AppModuleImpl(this)

        scope.launch {
            appModule.appPreferences.preload()
        }
    }

    private val mConnection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, service: IBinder?) {
            Log.d(TAG, "onServiceConnected")
            isBinding = false
            Companion.service = Messenger(service)
            isBound = true
            // notify current running activity that service is connected
            val notifyIntent = Intent(applicationContext, MainActivity::class.java).apply {
                action = Intent.ACTION_VIEW
                addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_NEW_TASK)
                putExtra("ForegroundServiceBound", true)
            }
            startActivity(notifyIntent)
        }

        override fun onServiceDisconnected(name: ComponentName?) {
            Log.d(TAG, "onServiceDisconnected")
            service = null
            isBound = false
        }
    }

    // start and bind to service
    fun bindService() {
        // onStart can run while the app is still considered background (for
        // example when the PC launches it over adb with the screen off).
        // startService throws there; binding alone is enough to reach the
        // service, and MainActivity retries on resume.
        if (isBound || isBinding) return

        val intent = Intent(this, ForegroundService::class.java).apply {
            action = BIND_SERVICE_ACTION
        }
        isBinding = true
        try {
            startService(intent)
        } catch (e: IllegalStateException) {
            Log.w(TAG, "startService not allowed in background: ${e.message}")
        }
        if (!bindService(intent, mConnection, BIND_AUTO_CREATE)) {
            isBinding = false
        }
    }

    fun unBindService() {
        unbindService(mConnection)
        service = null
        isBound = false
        isBinding = false
    }
}