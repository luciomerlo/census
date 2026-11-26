package com.censusindex.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import kotlin.concurrent.thread

/**
 * Publica la API HTTP v1 (docs/MINIMO.md) del indice del telefono. La PC la
 * consulta por Tailscale (`http://<ip-tailscale-del-telefono>:8787/v1/...`
 * con `Authorization: Bearer <token>`) o baja el snapshot cuando esta prendido.
 * Es opcional y visible: foreground con notificacion.
 */
class ServeService : Service() {
    override fun onBind(i: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel("serve", "Servidor CensusIndex", NotificationManager.IMPORTANCE_LOW))
        val n = Notification.Builder(this, "serve")
            .setContentTitle("CensusIndex compartiendo el indice (solo lectura)")
            .setSmallIcon(android.R.drawable.ic_menu_search)
            .setOngoing(true)
            .build()
        startForeground(1, n)
        val h = Indexer.handle(this)
        val token = Indexer.token(this)
        thread(name = "censusindex-http") {
            // Con LAN exige token (contrato). Puerto ocupado => devuelve error y sale.
            Native.nativeServe(h, PORT, true, token)
            stopSelf()
        }
        return START_STICKY
    }

    companion object {
        const val PORT = 8787
        fun start(ctx: Context) = ctx.startForegroundService(Intent(ctx, ServeService::class.java))
        fun stop(ctx: Context) = ctx.stopService(Intent(ctx, ServeService::class.java))
    }
}
