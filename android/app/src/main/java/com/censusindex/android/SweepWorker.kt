package com.censusindex.android

import android.content.Context
import androidx.work.Worker
import androidx.work.WorkerParameters

class SweepWorker(ctx: Context, params: WorkerParameters) : Worker(ctx, params) {
    override fun doWork(): Result {
        if (!Perms.hasAllFiles()) return Result.success()
        val r = Indexer.sweep(applicationContext)
        return if (r.has("error")) Result.retry() else Result.success()
    }
}
