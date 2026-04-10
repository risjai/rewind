import { cn } from '@/lib/utils'

interface ScoreBadgeProps {
  score: number | null
  className?: string
}

export function ScoreBadge({ score, className }: ScoreBadgeProps) {
  if (score === null || score === undefined) {
    return (
      <span className={cn('inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium bg-neutral-800 text-neutral-500', className)}>
        --
      </span>
    )
  }

  const bg = score >= 0.8
    ? 'bg-green-500/15 text-green-400'
    : score >= 0.6
      ? 'bg-amber-500/15 text-amber-400'
      : 'bg-red-500/15 text-red-400'

  return (
    <span className={cn('inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium', bg, className)}>
      {score.toFixed(2)}
    </span>
  )
}
