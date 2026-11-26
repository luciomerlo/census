package com.censusindex.android

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

class BootReceiver : BroadcastReceiver() {
    override fun onReceive(ctx: Context, intent: Intent) {
        if (Perms.hasAllFiles()) {
            MediaChangeJob.schedule(ctx)
            Indexer.schedulePeriodic(ctx)
        }
    }
}
