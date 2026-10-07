// Small numeric/filter/sort oracle, independent of the Wikipedia timing benchmark.
import org.apache.lucene.analysis.standard.StandardAnalyzer;
import org.apache.lucene.document.*;
import org.apache.lucene.index.*;
import org.apache.lucene.search.*;
import org.apache.lucene.store.ByteBuffersDirectory;
import org.apache.lucene.util.BytesRef;

public class NumericParity {
  public static void main(String[] args) throws Exception {
    try (var dir = new ByteBuffersDirectory()) {
      try (var writer = new IndexWriter(dir, new IndexWriterConfig(new StandardAnalyzer()))) {
        Long[] longs = {Long.MAX_VALUE, -5L, null, 9007199254740993L, -5L, Long.MIN_VALUE};
        Double[] doubles = {-0.0, 0.0, null, 1.25, -2.5, 1.25};
        String[] tags = {"z", "b", null, "a", "c", "b"};
        for (int i = 0; i < longs.length; i++) {
          var doc = new Document();
          doc.add(new StoredField("id", String.valueOf((char) ('a' + i))));
          if (longs[i] != null) {
            doc.add(new LongPoint("n", longs[i]));
            doc.add(new NumericDocValuesField("n", longs[i]));
            doc.add(new DoublePoint("price", doubles[i]));
            doc.add(new DoubleDocValuesField("price", doubles[i]));
            doc.add(new SortedDocValuesField("tag", new BytesRef(tags[i])));
          }
          writer.addDocument(doc);
        }
      }
      try (var reader = DirectoryReader.open(dir)) {
        var searcher = new IndexSearcher(reader);
        var number = new SortField("n", SortField.Type.LONG);
        number.setMissingValue(Long.MAX_VALUE);
        var price = new SortField("price", SortField.Type.DOUBLE);
        price.setMissingValue(Double.POSITIVE_INFINITY);
        var desc = new SortField("price", SortField.Type.DOUBLE, true);
        desc.setMissingValue(Double.POSITIVE_INFINITY);
        var tag = new SortField("tag", SortField.Type.STRING);
        tag.setMissingValue(SortField.STRING_LAST);
        emit(searcher, "long-sort", new MatchAllDocsQuery(), number);
        emit(searcher, "long-range", LongPoint.newRangeQuery("n", 9007199254740993L, Long.MAX_VALUE), number);
        emit(searcher, "double-range", DoublePoint.newRangeQuery("price", 0.0, 1.25), price);
        emit(searcher, "negative-zero", DoublePoint.newExactQuery("price", -0.0), price);
        var filtered = new BooleanQuery.Builder()
            .add(new MatchAllDocsQuery(), BooleanClause.Occur.MUST)
            .add(LongPoint.newRangeQuery("n", -5, 9007199254740993L), BooleanClause.Occur.FILTER).build();
        emit(searcher, "filter-keyword", filtered, tag);
        emit(searcher, "descending-missing", new MatchAllDocsQuery(), desc);
      }
    }
  }
  static void emit(IndexSearcher searcher, String name, Query query, SortField field) throws Exception {
    var hits = searcher.search(query, 20, new Sort(field));
    var ids = new java.util.ArrayList<String>();
    for (var hit : hits.scoreDocs) ids.add(searcher.storedFields().document(hit.doc).get("id"));
    System.out.println(name + "\t" + hits.totalHits.value() + "\t" + String.join(",", ids));
  }
}
