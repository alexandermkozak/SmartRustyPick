<script lang="ts" setup>
/**
 * Records per group, as columns.
 *
 * Two extremes could never show skew: a smallest and a largest say nothing
 * about whether the file is one long tail or one outlier, and it is the shape
 * that says whether the hash is doing its job. One column standing far out to
 * the right is what an operator is looking for, and it is visible here and
 * nowhere in a table of numbers.
 *
 * The buckets are computed by the server - equal width over the record counts,
 * capped - so a file with a modulus of 65,536 still draws in one small reply.
 */
import {computed} from 'vue'
import {count} from '@shared/format'

interface Bucket {
  min: number
  max: number
  groups: number
}

const props = defineProps<{buckets: Bucket[]; groups: number}>()

const tallest = computed(() => Math.max(1, ...props.buckets.map((bucket) => bucket.groups)))

const columns = computed(() =>
  props.buckets.map((bucket) => ({
    ...bucket,
    // A share of the tallest column rather than of the total: the point is the
    // relative shape, and one bucket usually holds most of the groups.
    height: Math.round((bucket.groups / tallest.value) * 100),
    label:
      bucket.min === bucket.max
        ? `${count(bucket.min)} records`
        : `${count(bucket.min)}–${count(bucket.max)} records`,
  })),
)
</script>

<template>
  <figure v-if="columns.length" class="distribution">
    <div class="bars">
      <div
        v-for="bucket in columns"
        :key="bucket.min"
        :title="`${count(bucket.groups)} groups hold ${bucket.label}`"
        class="bar"
      >
        <span :style="{height: `${bucket.height}%`}" class="fill"></span>
      </div>
    </div>
    <figcaption>
      Groups by records held, {{ count(columns[0].min) }} on the left to
      {{ count(columns[columns.length - 1].max) }} on the right, over
      {{ count(props.groups) }} groups.
    </figcaption>
  </figure>
</template>
