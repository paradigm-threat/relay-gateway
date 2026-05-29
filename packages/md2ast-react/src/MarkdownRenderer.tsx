'use client'

import React, { useEffect, useMemo, useState } from 'react'
import type { AnchorHTMLAttributes, ComponentType, MouseEventHandler } from 'react'
import { defaultHtmlOverrides } from './defaultOverrides.js'
import { renderMdAst, type MdNode, type MdOverrides } from './mdAst.js'
import {
  diagnoseMarkdownError,
  validateMarkdownSource,
  type MarkdownCompileDiagnostics,
} from './diagnostics.js'
import { MarkdownCompileErrorPanel } from './MarkdownCompileErrorPanel.js'
import { normalizeParadigmMarkdown } from './normalizeParadigmMarkdown.js'
import { parseMarkdownInternal } from './wasmLoader.js'

export type { MdNode, MdOverrides }
export type { MarkdownCompileDiagnostics } from './diagnostics.js'

export type MarkdownOverrideComponent = ComponentType<Record<string, unknown>>

export interface MarkdownRendererProps {
  content: string
  overrides?: MdOverrides
  className?: string
  /** Shown in compile error panel (e.g. repo path). */
  sourcePath?: string
  anchorProps?: AnchorHTMLAttributes<HTMLAnchorElement>
  anchorOnClick?: MouseEventHandler<HTMLAnchorElement>
  /** Called when render finishes (success or error panel). */
  onReady?: () => void
}

export function MarkdownRenderer ({
  content,
  overrides = {},
  className = 'relay-markdown',
  sourcePath,
  anchorProps,
  anchorOnClick,
  onReady,
}: MarkdownRendererProps) {
  const [nodes, setNodes] = useState<MdNode[] | null>(null)
  const [diagnostics, setDiagnostics] = useState<MarkdownCompileDiagnostics | null>(null)

  const allOverrides = useMemo(
    () => ({ ...defaultHtmlOverrides, ...overrides }),
    [overrides],
  )

  const allowedTags = useMemo(() => Object.keys(allOverrides), [allOverrides])

  const renderOpts = useMemo(
    () => ({ overrides: allOverrides, anchorProps, anchorOnClick }),
    [allOverrides, anchorProps, anchorOnClick],
  )

  const normalizedContent = useMemo(() => normalizeParadigmMarkdown(content), [content])

  useEffect(() => {
    let cancel = false
    ;(async () => {
      try {
        validateMarkdownSource(normalizedContent, allowedTags)
        const parsed = await parseMarkdownInternal(normalizedContent, allowedTags)
        if (!cancel) {
          setNodes(parsed)
          setDiagnostics(null)
        }
      } catch (e) {
        if (!cancel) {
          setNodes(null)
          setDiagnostics(diagnoseMarkdownError(e, normalizedContent))
        }
      }
    })()
    return () => { cancel = true }
  }, [normalizedContent, allowedTags])

  useEffect(() => {
    if (nodes) onReady?.()
  }, [nodes, onReady])

  if (diagnostics) {
    return <MarkdownCompileErrorPanel diagnostics={diagnostics} sourcePath={sourcePath} />
  }
  if (!nodes) {
    return <div className="p-4 text-sm opacity-60">Loading markdown…</div>
  }

  return (
    <article className={className}>
      {renderMdAst(nodes, renderOpts)}
    </article>
  )
}
